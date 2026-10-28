//! State tests on an in-memory store in demo mode (no network).

use super::*;
use crate::account_form::Field;
use crate::worker::ErrorKind;
use avero_core::{NewAccount, NewMessage, ServerConfig};

/// Adds an account with an inbox holding `count` messages.
fn add_account_with_mail(store: &Store, email: &str, count: u32) -> AccountId {
    let server = ServerConfig {
        host: "imap.example.com".to_string(),
        port: 993,
        username: email.to_string(),
        security: avero_core::Security::Tls,
    };
    let account = store
        .add_account(NewAccount {
            name: email.to_string(),
            email: email.to_string(),
            imap: server.clone(),
            smtp: server,
            auth: avero_core::AuthKind::Password,
        })
        .unwrap();
    let inbox = store
        .upsert_folder(account.id, "INBOX", FolderRole::Inbox)
        .unwrap();
    store
        .upsert_folder(account.id, "Trash", FolderRole::Trash)
        .unwrap();
    let messages: Vec<NewMessage> = (1..=count)
        .map(|uid| NewMessage {
            uid,
            message_id: Some(format!("<{uid}@{email}>")),
            subject: format!("Sujet {uid}"),
            from_name: None,
            from_addr: "x@y.z".to_string(),
            date: 1_700_000_000 + i64::from(uid),
            seen: false,
            has_attachments: false,
            ..Default::default()
        })
        .collect();
    store.insert_messages(inbox.id, &messages).unwrap();
    account.id
}

fn app_with(setup: impl FnOnce(&Store)) -> App {
    let store = Store::open_in_memory().unwrap();
    setup(&store);
    let (app, _task) = App::new(store, Backend::Demo, Settings::default(), None);
    app
}

fn two_accounts() -> App {
    app_with(|s| {
        add_account_with_mail(s, "a@x.fr", 3);
        add_account_with_mail(s, "b@x.fr", 2);
    })
}

fn ids(app: &App) -> (AccountId, AccountId) {
    (app.accounts[0].id, app.accounts[1].id)
}

/// Selects an item and delivers the parsed body, as the background
/// parsing task would.
fn open_parsed(app: &mut App, index: usize) {
    let _ = app.update(Message::MessageSelected(index));
    let row = app.reading.as_ref().unwrap().focused().item.row_ids[0];
    let (raw, _) = app.store.body(row).unwrap().expect("a cached body");
    let opened = crate::background::OpenedBody::parse(&raw);
    let _ = app.update(Message::BodyParsed(row, Ok(Arc::new(opened))));
}

/// Marks every account as synced once, with an archive folder or not.
fn synced(app: &mut App, archive: bool) {
    for id in app.accounts.iter().map(|a| a.id).collect::<Vec<_>>() {
        let _ = app.update(Message::Worker(id, Event::Synced(Ok(archive))));
    }
}

#[test]
fn starts_on_all_accounts_unified_inbox() {
    let app = two_accounts();
    let (a, b) = ids(&app);
    assert_eq!(app.filter, AccountFilter::All);
    assert_eq!(app.folder, FolderRole::Inbox);
    assert_eq!(app.items.len(), 5);
    assert_eq!(app.unread[&a], 3);
    assert_eq!(app.unread[&b], 2);
}

#[test]
fn selecting_account_filters_list_and_clears_selection() {
    let mut app = two_accounts();
    let (a, _) = ids(&app);
    let _ = app.update(Message::MessageSelected(1));
    assert_eq!(app.selected_index(), Some(1));
    let _ = app.update(Message::FilterSelected(AccountFilter::Only(a)));
    assert_eq!(app.items.len(), 3);
    assert!(app.items.iter().all(|i| i.accounts.contains(&a)));
    assert_eq!(app.selected, None);
}

#[test]
fn opening_marks_read_locally_and_keeps_selection() {
    let mut app = two_accounts();
    let (a, _) = ids(&app);
    let rows = app.items[0].row_ids.clone();
    let _ = app.update(Message::MessageSelected(0));
    assert!(app.items[0].seen);
    assert_eq!(app.selected.as_ref(), Some(&rows));
    assert_eq!(app.unread[&a] + app.unread[&ids(&app).1], 4);
    // No cached body: the reader waits for the worker.
    assert!(matches!(
        app.reading.as_ref().unwrap().body(),
        Body::Loading
    ));
    let _ = app.update(Message::MarkUnread);
    assert!(!app.items[0].seen);
}

#[test]
fn body_failure_is_shown() {
    let mut app = two_accounts();
    let (a, _) = ids(&app);
    let _ = app.update(Message::MessageSelected(0));
    let row = app.items[0].row_ids[0];
    let _ = app.update(Message::Worker(
        a,
        Event::Body {
            row_id: row,
            result: Err("hors ligne".into()),
        },
    ));
    assert!(matches!(&app.reading.as_ref().unwrap().body(), Body::Failed(e) if e == "hors ligne"));
}

#[test]
fn j_and_k_move_the_selection() {
    let mut app = two_accounts();
    let key = |c: &str| {
        Message::Key(
            keyboard::Key::Character(c.into()),
            keyboard::Modifiers::default(),
        )
    };
    let _ = app.update(key("j"));
    assert_eq!(app.selected_index(), Some(0));
    let _ = app.update(key("j"));
    let _ = app.update(key("j"));
    assert_eq!(app.selected_index(), Some(2));
    let _ = app.update(key("k"));
    assert_eq!(app.selected_index(), Some(1));
}

#[test]
fn delete_removes_locally_and_selects_next() {
    let mut app = two_accounts();
    let _ = app.update(Message::MessageSelected(1));
    let removed = app.items[1].row_ids.clone();
    let _ = app.update(Message::Delete);
    assert_eq!(app.items.len(), 4);
    assert!(app.items.iter().all(|i| i.row_ids != removed));
    assert_eq!(app.selected_index(), Some(1));
    assert!(app.toast.is_some());
}

#[test]
fn delete_needs_a_selection_and_a_trash() {
    let mut app = two_accounts();
    let _ = app.update(Message::FolderSelected(FolderRole::Trash));
    assert!(app.items.is_empty());
    assert!(!app.can_delete());

    // An account without trash folder cannot delete.
    let mut app = app_with(|s| {
        let other = add_account_with_mail(s, "a@x.fr", 1);
        let account = s.accounts().unwrap().remove(0);
        let inbox = s.folder_by_role(other, FolderRole::Inbox).unwrap().unwrap();
        // Same messages, on a second account that has no trash.
        let lonely = s
            .add_account(avero_core::NewAccount {
                name: "b".into(),
                email: "b@x.fr".into(),
                imap: account.imap.clone(),
                smtp: account.smtp.clone(),
                auth: account.auth,
            })
            .unwrap();
        let folder = s
            .upsert_folder(lonely.id, "INBOX", FolderRole::Inbox)
            .unwrap();
        let message = NewMessage {
            uid: 5,
            message_id: Some("<only-b@x>".into()),
            subject: "Seul".into(),
            from_name: None,
            from_addr: "x@y.z".into(),
            date: 1_900_000_000,
            seen: false,
            has_attachments: false,
            ..Default::default()
        };
        s.insert_messages(folder.id, &[message]).unwrap();
        let _ = inbox;
    });
    synced(&mut app, true);
    // The newest item is the one of the account without trash.
    let _ = app.update(Message::MessageSelected(0));
    assert_eq!(app.items[0].subject, "Seul");
    assert!(!app.can_delete());
    assert_eq!(app.delete_problem(), Some("Aucune corbeille sur ce compte"));
    let _ = app.update(Message::Delete);
    assert_eq!(app.items.len(), 2);
}

#[test]
fn failed_delete_brings_the_message_back() {
    let mut app = two_accounts();
    let _ = app.update(Message::MessageSelected(0));
    let rows = app.items[0].row_ids.clone();
    let _ = app.update(Message::Delete);
    assert_eq!(app.items.len(), 4, "hidden at once");
    assert!(
        app.store.message_refs(&rows).unwrap().len() == 1,
        "not deleted yet"
    );

    let error = AccountError::new(ErrorKind::Network, "Connexion impossible");
    let _ = app.update(Message::Moved(rows.clone(), Event::Done(Err(error))));
    assert_eq!(app.items.len(), 5);
    assert!(app.items.iter().any(|i| i.row_ids == rows));
    let toast = app.toast.as_ref().unwrap();
    assert!(toast.error && toast.text == "Connexion impossible");

    // A worker that vanished also brings it back.
    let _ = app.update(Message::MessageSelected(0));
    let _ = app.update(Message::Delete);
    let _ = app.update(Message::Moved(rows.clone(), Event::Gone));
    assert_eq!(app.items.len(), 5);

    // Confirmed: the rows are gone for good.
    let _ = app.update(Message::MessageSelected(0));
    let _ = app.update(Message::Delete);
    let _ = app.update(Message::Moved(rows.clone(), Event::Done(Ok(()))));
    assert_eq!(app.items.len(), 4);
    assert!(app.store.message_refs(&rows).unwrap().is_empty());
}

#[test]
fn deleting_from_the_trash_asks_then_deletes_for_good() {
    let mut app = app_with(|s| {
        let id = add_account_with_mail(s, "a@x.fr", 0);
        let trash = s.folder_by_role(id, FolderRole::Trash).unwrap().unwrap();
        let message = NewMessage {
            uid: 7,
            message_id: Some("<7@a>".into()),
            subject: "Vieux".into(),
            from_name: None,
            from_addr: "x@y.z".into(),
            date: 1,
            seen: true,
            has_attachments: false,
            ..Default::default()
        };
        s.insert_messages(trash.id, &[message]).unwrap();
    });
    synced(&mut app, true);
    let _ = app.update(Message::FolderSelected(FolderRole::Trash));
    let _ = app.update(Message::MessageSelected(0));
    assert!(app.can_delete());
    assert!(app.selection_in_trash());
    let rows = app.items[0].row_ids.clone();
    let _ = app.update(Message::Delete);
    assert!(matches!(
        app.overlay,
        Some(Overlay::ConfirmDeleteForever(_))
    ));
    assert_eq!(app.items.len(), 1, "nothing before the confirmation");
    let _ = app.update(Message::ConfirmDeleteForever);
    assert!(app.overlay.is_none());
    assert!(app.items.is_empty());
    let _ = app.update(Message::Moved(rows.clone(), Event::Done(Ok(()))));
    assert!(app.store.message_refs(&rows).unwrap().is_empty());
}

#[test]
fn archive_waits_for_a_known_archive_folder() {
    let mut app = two_accounts();
    let (a, _) = ids(&app);
    let _ = app.update(Message::FilterSelected(AccountFilter::Only(a)));
    let _ = app.update(Message::MessageSelected(0));
    assert!(!app.can_archive(), "unknown before the first sync");
    assert_eq!(
        app.archive_problem(),
        Some("Disponible après la première synchronisation")
    );
    let _ = app.update(Message::Worker(a, Event::Synced(Ok(false))));
    assert!(!app.can_archive());
    assert_eq!(
        app.archive_problem(),
        Some("Aucun dossier d'archives sur ce compte")
    );
    let before = app.items.len();
    let _ = app.update(Message::Archive);
    assert_eq!(app.items.len(), before);
    let _ = app.update(Message::Worker(a, Event::Synced(Ok(true))));
    assert!(app.can_archive());
    let _ = app.update(Message::Archive);
    assert_eq!(app.items.len(), before - 1);
}

#[test]
fn archive_tab_only_when_an_archive_folder_is_synced() {
    let mut app = two_accounts();
    assert!(!app.archive_view, "Gmail-like: no Archive folder");
    let a = app.accounts[0].id;
    app.store
        .upsert_folder(a, "Archives", FolderRole::Archive)
        .unwrap();
    let _ = app.update(Message::Worker(a, Event::Synced(Ok(true))));
    assert!(app.archive_view);
}

#[test]
fn progress_reloads_are_throttled() {
    let mut app = two_accounts();
    let (a, _) = ids(&app);
    let inbox = app
        .store
        .folder_by_role(a, FolderRole::Inbox)
        .unwrap()
        .unwrap();
    let add = |app: &App, uid: u32| {
        let message = NewMessage {
            uid,
            message_id: Some(format!("<{uid}@new>")),
            subject: format!("Nouveau {uid}"),
            from_name: None,
            from_addr: "x@y.z".to_string(),
            date: 1_800_000_000 + i64::from(uid),
            seen: false,
            has_attachments: false,
            ..Default::default()
        };
        app.store.insert_messages(inbox.id, &[message]).unwrap();
    };
    add(&app, 100);
    let _ = app.update(Message::Worker(a, Event::Progress));
    assert_eq!(app.items.len(), 6);
    add(&app, 101);
    let _ = app.update(Message::Worker(a, Event::Progress));
    let _ = app.update(Message::Worker(a, Event::Progress));
    assert_eq!(app.items.len(), 6, "too soon: waits for its turn");
    let _ = app.update(Message::ReloadDue);
    assert_eq!(app.items.len(), 7);
    // The end of the sync always reloads.
    add(&app, 102);
    let _ = app.update(Message::Worker(a, Event::Synced(Ok(true))));
    assert_eq!(app.items.len(), 8);
}

#[test]
fn lost_worker_ends_sync_and_loading() {
    let mut app = two_accounts();
    let (a, _) = ids(&app);
    let _ = app.update(Message::Refresh);
    let _ = app.update(Message::MessageSelected(0));
    let _ = app.update(Message::Worker(a, Event::Gone));
    assert!(!app.status[&a].syncing);
    assert!(matches!(
        app.reading.as_ref().unwrap().body(),
        Body::Failed(_)
    ));
}

#[test]
fn parse_failure_is_shown_in_the_reader() {
    let mut app = two_accounts();
    let _ = app.update(Message::MessageSelected(0));
    let row = app.items[0].row_ids[0];
    let _ = app.update(Message::BodyParsed(
        row,
        Err("Ce message n'a pas pu être analysé".into()),
    ));
    assert!(matches!(
        &app.reading.as_ref().unwrap().body(),
        Body::Failed(e) if e.contains("analysé")
    ));
}

#[test]
fn sync_error_is_recorded_per_account_and_cleared_on_success() {
    let mut app = two_accounts();
    let (a, b) = ids(&app);
    let _ = app.update(Message::Refresh);
    assert!(app.is_syncing());
    let error = AccountError::new(ErrorKind::Auth, "refusé");
    let _ = app.update(Message::Worker(a, Event::Synced(Err(error.clone()))));
    assert_eq!(app.status[&a].error.as_ref(), Some(&error));
    assert!(app.status[&b].error.is_none());
    let _ = app.update(Message::Worker(a, Event::Synced(Ok(true))));
    assert!(app.status[&a].error.is_none());
    assert!(!app.status[&a].syncing);
}

#[test]
fn progress_reloads_the_list() {
    let mut app = two_accounts();
    let (a, _) = ids(&app);
    let inbox = app
        .store
        .folder_by_role(a, FolderRole::Inbox)
        .unwrap()
        .unwrap();
    let new_message = NewMessage {
        uid: 99,
        message_id: Some("<99@a>".to_string()),
        subject: "Nouveau".to_string(),
        from_name: None,
        from_addr: "x@y.z".to_string(),
        date: 1_800_000_000,
        seen: false,
        has_attachments: false,
        ..Default::default()
    };
    app.store.insert_messages(inbox.id, &[new_message]).unwrap();
    let _ = app.update(Message::Worker(a, Event::Progress));
    assert_eq!(app.items.len(), 6);
    assert_eq!(app.items[0].subject, "Nouveau");
}

#[test]
fn load_more_appends_next_page_and_reload_keeps_pages() {
    let mut app = app_with(|s| {
        add_account_with_mail(s, "a@x.fr", 450);
    });
    let a = app.accounts[0].id;
    assert_eq!(app.items.len(), 200);
    let _ = app.update(Message::LoadMore);
    assert_eq!(app.items.len(), 400);
    let _ = app.update(Message::Worker(a, Event::Progress));
    assert_eq!(app.items.len(), 400);
    let _ = app.update(Message::LoadMore);
    assert_eq!(app.items.len(), 450);
}

#[test]
fn search_applies_after_debounce_or_submit() {
    let mut app = two_accounts();
    let _ = app.update(Message::SearchChanged("Sujet 3".into()));
    assert_eq!(app.items.len(), 5, "not applied before the delay");
    let _ = app.update(Message::SearchDebounced(0));
    assert_eq!(app.items.len(), 5, "stale generation ignored");
    let generation = app.search_generation;
    let _ = app.update(Message::SearchDebounced(generation));
    assert_eq!(app.items.len(), 1);
    assert_eq!(app.search_query.as_deref(), Some("Sujet 3"));
    let _ = app.update(Message::SearchCleared);
    assert_eq!(app.items.len(), 5);
    let _ = app.update(Message::SearchChanged("Sujet".into()));
    let _ = app.update(Message::SearchSubmitted);
    assert_eq!(app.items.len(), 5);
}

#[test]
fn escape_closes_composer_then_search() {
    let mut app = two_accounts();
    let _ = app.update(Message::Compose(ComposeMsg::New));
    assert!(app.composer.is_some());
    let escape = Message::Key(
        keyboard::Key::Named(keyboard::key::Named::Escape),
        keyboard::Modifiers::default(),
    );
    let _ = app.update(escape.clone());
    assert!(app.composer.is_none());
    let _ = app.update(Message::SearchChanged("x".into()));
    let _ = app.update(escape);
    assert!(app.search_text.is_empty());
}

#[test]
fn new_message_uses_filtered_account() {
    let mut app = two_accounts();
    let (_, b) = ids(&app);
    let _ = app.update(Message::FilterSelected(AccountFilter::Only(b)));
    let _ = app.update(Message::Key(
        keyboard::Key::Character("n".into()),
        keyboard::Modifiers::COMMAND,
    ));
    assert_eq!(app.composer.as_ref().unwrap().from, b);
}

#[test]
fn send_validates_then_closes_on_success() {
    let mut app = two_accounts();
    let (a, _) = ids(&app);
    let _ = app.update(Message::Compose(ComposeMsg::New));
    let _ = app.update(Message::Compose(ComposeMsg::Send));
    let composer = app.composer.as_ref().unwrap();
    assert_eq!(
        composer.error.as_deref(),
        Some("Ajoutez au moins un destinataire.")
    );
    let _ = app.update(Message::Compose(ComposeMsg::To("paul@ex.fr".into())));
    let _ = app.update(Message::Compose(ComposeMsg::Send));
    assert!(app.composer.as_ref().unwrap().sending);
    let _ = app.update(Message::Worker(a, Event::Sent(Err("refusé".into()))));
    let composer = app.composer.as_ref().unwrap();
    assert!(!composer.sending);
    assert_eq!(composer.error.as_deref(), Some("refusé"));
    let _ = app.update(Message::Compose(ComposeMsg::Send));
    let _ = app.update(Message::Worker(a, Event::Sent(Ok(None))));
    assert!(app.composer.is_none());
    assert_eq!(app.toast.as_ref().unwrap().text, "Message envoyé");
}

#[test]
fn reply_needs_a_loaded_body() {
    let mut app = app_with(|s| {
        crate::demo::seed(s);
    });
    let _ = app.update(Message::MessageSelected(0));
    assert!(matches!(
        app.reading.as_ref().unwrap().body(),
        Body::Loading
    ));
    let _ = app.update(Message::Answer(Mode::Reply));
    assert!(app.composer.is_none());
    open_parsed(&mut app, 0);
    assert!(matches!(
        app.reading.as_ref().unwrap().body(),
        Body::Ready { .. }
    ));
    let _ = app.update(Message::Answer(Mode::Reply));
    let composer = app.composer.as_ref().unwrap();
    assert!(composer.subject.starts_with("Re: "));
    assert!(composer.in_reply_to.is_some());
    assert_eq!(composer.from, app.items[0].accounts[0]);
    assert!(composer.body.text().contains("a écrit :"));
}

#[test]
fn forward_reattaches_original_files() {
    let mut app = app_with(|s| {
        crate::demo::seed(s);
    });
    let index = app.items.iter().position(|i| i.has_attachments).unwrap();
    open_parsed(&mut app, index);
    let _ = app.update(Message::Answer(Mode::Forward));
    let composer = app.composer.as_ref().unwrap();
    assert_eq!(composer.attachments.len(), 1);
    assert!(composer.subject.starts_with("Fwd: "));
}

#[test]
fn add_account_flow_checks_login_before_saving() {
    let mut app = app_with(|_| {});
    let _ = app.update(Message::Add(AddMsg::Open(None)));
    let _ = app.update(Message::Add(AddMsg::Field(
        Field::Email,
        "jean@gmail.com".into(),
    )));
    let _ = app.update(Message::Add(AddMsg::SubmitEmail));
    // `known()` answers through a task; deliver it by hand.
    let generation = form(&app).generation;
    let config = avero_autoconfig::known("jean@gmail.com");
    let _ = app.update(Message::Add(AddMsg::Detected(generation, config)));
    // No OAuth client in demo mode: the assistant first, then the app password.
    let _ = app.update(Message::Add(AddMsg::UsePassword));
    assert_eq!(form(&app).stage, crate::account_form::Stage::Credentials);
    assert!(form(&app).shows_password());
    let _ = app.update(Message::Add(AddMsg::Field(
        Field::Password,
        "abcd efgh ijkl mnop".into(),
    )));
    let _ = app.update(Message::Add(AddMsg::SubmitPassword));
    assert_eq!(form(&app).stage, crate::account_form::Stage::Checking);
    assert!(app.accounts.is_empty(), "nothing saved before the check");

    let generation = form(&app).generation;
    let refused = Err(LoginError::Refused);
    let _ = app.update(Message::Add(AddMsg::Checked(generation, refused)));
    assert!(app.accounts.is_empty());
    assert!(
        form(&app)
            .error
            .as_ref()
            .unwrap()
            .contains("mot de passe d'application")
    );

    let _ = app.update(Message::Add(AddMsg::SubmitPassword));
    let generation = form(&app).generation;
    let _ = app.update(Message::Add(AddMsg::Checked(generation, Ok(()))));
    assert_eq!(app.accounts.len(), 1);
    assert!(app.overlay.is_none());
    assert_eq!(app.accounts[0].name, "jean");
    assert!(app.status[&app.accounts[0].id].syncing);
}

#[test]
fn stale_check_results_are_ignored() {
    let mut app = app_with(|_| {});
    let _ = app.update(Message::Add(AddMsg::Open(None)));
    let _ = app.update(Message::Add(AddMsg::Field(
        Field::Email,
        "a@orange.fr".into(),
    )));
    let _ = app.update(Message::Add(AddMsg::SubmitEmail));
    let generation = form(&app).generation;
    let _ = app.update(Message::Add(AddMsg::Detected(
        generation,
        avero_autoconfig::known("a@orange.fr"),
    )));
    let _ = app.update(Message::Add(AddMsg::Field(Field::Password, "pw".into())));
    let _ = app.update(Message::Add(AddMsg::SubmitPassword));
    let old = form(&app).generation;
    let _ = app.update(Message::Add(AddMsg::Back));
    let _ = app.update(Message::Add(AddMsg::Checked(old, Ok(()))));
    assert!(app.accounts.is_empty());
}

#[test]
fn duplicate_account_is_refused() {
    let mut app = app_with(|s| {
        add_account_with_mail(s, "a@orange.fr", 1);
    });
    let _ = app.update(Message::Add(AddMsg::Open(None)));
    let _ = app.update(Message::Add(AddMsg::Field(
        Field::Email,
        "A@orange.fr".into(),
    )));
    let _ = app.update(Message::Add(AddMsg::SubmitEmail));
    let generation = form(&app).generation;
    let _ = app.update(Message::Add(AddMsg::Detected(
        generation,
        avero_autoconfig::known("a@orange.fr"),
    )));
    let _ = app.update(Message::Add(AddMsg::Field(Field::Password, "pw".into())));
    let _ = app.update(Message::Add(AddMsg::SubmitPassword));
    let generation = form(&app).generation;
    let _ = app.update(Message::Add(AddMsg::Checked(generation, Ok(()))));
    assert_eq!(app.accounts.len(), 1);
    assert_eq!(
        form(&app).error.as_deref(),
        Some("Ce compte est déjà dans Avero.")
    );
}

#[test]
fn google_button_without_client_opens_the_assistant() {
    use crate::account_form::Stage;
    let mut app = app_with(|_| {});
    let _ = app.update(Message::Add(AddMsg::Open(Some(Provider::Google))));
    assert_eq!(form(&app).stage, Stage::Setup(Provider::Google));
    // The app password stays possible: the address comes first.
    let _ = app.update(Message::Add(AddMsg::UsePassword));
    assert_eq!(form(&app).stage, Stage::Email);
    assert!(form(&app).note.as_deref().unwrap().contains("Gmail"));
}

#[test]
fn gmail_address_without_client_opens_the_assistant() {
    use crate::account_form::Stage;
    let mut app = app_with(|_| {});
    let _ = app.update(Message::Add(AddMsg::Open(None)));
    let _ = app.update(Message::Add(AddMsg::Field(
        Field::Email,
        "jean@gmail.com".into(),
    )));
    let _ = app.update(Message::Add(AddMsg::SubmitEmail));
    let generation = form(&app).generation;
    let _ = app.update(Message::Add(AddMsg::Detected(
        generation,
        avero_autoconfig::known("jean@gmail.com"),
    )));
    assert_eq!(form(&app).stage, Stage::Setup(Provider::Google));
    let _ = app.update(Message::Add(AddMsg::UsePassword));
    assert_eq!(form(&app).stage, Stage::Credentials);
    assert!(form(&app).shows_password());
    // ... and back to the assistant from the credentials.
    let _ = app.update(Message::Add(AddMsg::ShowSetup(Provider::Google)));
    assert_eq!(form(&app).stage, Stage::Setup(Provider::Google));
}

const GOOGLE_ID: &str = "1234567890-abcdef.apps.googleusercontent.com";

fn fill_google_setup(app: &mut App) {
    let _ = app.update(Message::Add(AddMsg::Field(
        Field::ClientId,
        GOOGLE_ID.into(),
    )));
    let _ = app.update(Message::Add(AddMsg::Field(
        Field::ClientSecret,
        "GOCSPX-abcdefghijkl".into(),
    )));
}

#[test]
fn assistant_saves_nothing_in_demo_mode() {
    let mut app = app_with(|_| {});
    let _ = app.update(Message::Add(AddMsg::Open(Some(Provider::Google))));
    fill_google_setup(&mut app);
    let _ = app.update(Message::Add(AddMsg::SaveSetup));
    assert!(form(&app).error.as_deref().unwrap().contains("Mode démo"));
}

#[test]
fn assistant_saves_the_client_then_signs_in() {
    use crate::account_form::Stage;
    let dir = std::env::temp_dir().join(format!("avero-setup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let backend = Backend::Live {
        db_path: dir.join("unused.db"),
        secrets: Arc::new(avero_core::secrets::MemorySecrets::default()),
        config_dir: dir.join("config"),
        workers: HashMap::new(),
    };
    let store = Store::open_in_memory().unwrap();
    let (mut app, _) = App::new(store, backend, Settings::default(), None);
    assert!(
        app.oauth_clients
            .get(&Provider::Google)
            .is_none_or(|c| !c.saved)
    );

    let _ = app.update(Message::Add(AddMsg::Open(Some(Provider::Google))));
    assert_eq!(form(&app).stage, Stage::Setup(Provider::Google));
    let _ = app.update(Message::Add(AddMsg::Field(
        Field::ClientId,
        "pas un id".into(),
    )));
    let _ = app.update(Message::Add(AddMsg::SaveSetup));
    assert!(form(&app).error.is_some());
    assert_eq!(form(&app).stage, Stage::Setup(Provider::Google));

    fill_google_setup(&mut app);
    let _ = app.update(Message::Add(AddMsg::SaveSetup));
    // Saved, then straight to the browser sign-in.
    assert_eq!(form(&app).stage, Stage::Authorizing(Provider::Google));
    assert!(form(&app).setup.client_secret.is_empty());
    let saved = avero_oauth::saved_client_config(Provider::Google, &dir.join("config")).unwrap();
    assert_eq!(saved.client_id, GOOGLE_ID);
    let info = &app.oauth_clients[&Provider::Google];
    assert!(info.saved);
    assert!(!info.masked_id.contains("abcdef.apps"));

    // Settings: change (prefilled, saving just closes), then remove.
    let _ = app.update(Message::Add(AddMsg::CancelWait));
    let _ = app.update(Message::Add(AddMsg::EditClient(Provider::Google)));
    assert!(form(&app).setup_only);
    assert_eq!(form(&app).setup.client_id, GOOGLE_ID);
    let _ = app.update(Message::Add(AddMsg::SaveSetup));
    assert!(app.overlay.is_none());
    let _ = app.update(Message::Add(AddMsg::RemoveClient(Provider::Google)));
    assert!(avero_oauth::saved_client_config(Provider::Google, &dir.join("config")).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn add_message_debug_hides_the_client_secret() {
    let message = AddMsg::Field(Field::ClientSecret, "GOCSPX-hidden".into());
    assert!(!format!("{message:?}").contains("hidden"));
}

fn form(app: &App) -> &AccountForm {
    match &app.overlay {
        Some(Overlay::AddAccount(form)) => form,
        other => panic!("no form: {other:?}"),
    }
}

#[test]
fn rename_and_delete_account() {
    let mut app = two_accounts();
    let (a, b) = ids(&app);
    let _ = app.update(Message::Account(AccountMsg::StartRename(a)));
    let _ = app.update(Message::Account(AccountMsg::RenameText("Perso".into())));
    let _ = app.update(Message::Account(AccountMsg::SaveRename));
    assert_eq!(app.accounts[0].name, "Perso");
    assert_eq!(app.store.accounts().unwrap()[0].name, "Perso");

    let _ = app.update(Message::FilterSelected(AccountFilter::Only(b)));
    let _ = app.update(Message::Account(AccountMsg::AskDelete(b)));
    let _ = app.update(Message::Account(AccountMsg::ConfirmDelete));
    assert_eq!(app.accounts.len(), 1);
    assert_eq!(app.filter, AccountFilter::All);
    assert_eq!(app.items.len(), 3);
    assert_eq!(app.store.accounts().unwrap().len(), 1);
}

#[test]
fn password_change_is_checked_then_clears_error() {
    let mut app = two_accounts();
    let (a, _) = ids(&app);
    let error = AccountError::new(ErrorKind::Auth, "refusé");
    let _ = app.update(Message::Worker(a, Event::Synced(Err(error))));
    let _ = app.update(Message::Account(AccountMsg::OpenPassword(a)));
    let _ = app.update(Message::Account(AccountMsg::SubmitPassword));
    match &app.overlay {
        Some(Overlay::Password(dialog)) => {
            assert_eq!(dialog.error.as_deref(), Some("Saisissez le mot de passe"));
        }
        other => panic!("{other:?}"),
    }
    let _ = app.update(Message::Account(AccountMsg::PasswordText("neuf".into())));
    let _ = app.update(Message::Account(AccountMsg::SubmitPassword));
    let _ = app.update(Message::Account(AccountMsg::PasswordChecked(1, Ok(()))));
    assert!(app.overlay.is_none());
    assert!(app.status[&a].error.is_none());
}

#[test]
fn theme_choice_switches_theme() {
    let mut app = two_accounts();
    let _ = app.update(Message::ThemeChosen(ThemeChoice::Dark));
    assert!(app.theme().extended_palette().is_dark);
    let _ = app.update(Message::ThemeChosen(ThemeChoice::System));
    let _ = app.update(Message::SystemTheme(iced::theme::Mode::Light));
    assert!(!app.theme().extended_palette().is_dark);
    let _ = app.update(Message::SystemTheme(iced::theme::Mode::Dark));
    assert!(app.theme().extended_palette().is_dark);
}

#[test]
fn sending_without_smtp_server_is_explained() {
    let mut app = app_with(|s| {
        let id = add_account_with_mail(s, "a@x.fr", 1);
        let account = s.accounts().unwrap().remove(0);
        let mut smtp = account.smtp.clone();
        smtp.host.clear();
        s.update_account(id, &account.name, &account.imap, &smtp, account.auth)
            .unwrap();
    });
    let _ = app.update(Message::Compose(ComposeMsg::New));
    let _ = app.update(Message::Compose(ComposeMsg::To("paul@ex.fr".into())));
    let _ = app.update(Message::Compose(ComposeMsg::Send));
    let composer = app.composer.as_ref().unwrap();
    assert!(!composer.sending);
    assert_eq!(composer.error.as_deref(), Some(crate::worker::SMTP_MISSING));
}

#[test]
fn servers_dialog_saves_smtp_settings() {
    let mut app = two_accounts();
    let (a, _) = ids(&app);
    let _ = app.update(Message::Account(AccountMsg::OpenServers(a)));
    let text =
        |field, value: &str| Message::Account(AccountMsg::ServerText(field, value.to_string()));
    let _ = app.update(text(ServerField::SmtpHost, ""));
    let _ = app.update(Message::Account(AccountMsg::SaveServers));
    assert!(matches!(&app.overlay, Some(Overlay::Servers(f)) if f.error.is_some()));
    let _ = app.update(text(ServerField::SmtpHost, " smtp.orange.fr "));
    let _ = app.update(text(ServerField::SmtpPort, "587"));
    let _ = app.update(Message::Account(AccountMsg::SaveServers));
    assert!(app.overlay.is_none());
    let saved = app.store.accounts().unwrap().remove(0);
    assert_eq!(saved.smtp.host, "smtp.orange.fr");
    assert_eq!(saved.smtp.port, 587);
    assert_eq!(saved.smtp.security, avero_core::Security::StartTls);
    assert_eq!(saved.smtp.username, "a@x.fr");
    assert_eq!(app.accounts[0].smtp.host, "smtp.orange.fr");
}

#[test]
fn smtp_found_fills_only_missing_settings() {
    let mut app = app_with(|s| {
        let id = add_account_with_mail(s, "a@orange.fr", 1);
        let account = s.accounts().unwrap().remove(0);
        let mut smtp = account.smtp.clone();
        smtp.host.clear();
        s.update_account(id, &account.name, &account.imap, &smtp, account.auth)
            .unwrap();
    });
    let id = app.accounts[0].id;
    let found = ServerConfig {
        host: "smtp.orange.fr".into(),
        port: 465,
        username: "ignored".into(),
        security: avero_core::Security::Tls,
    };
    let _ = app.update(Message::Account(AccountMsg::SmtpFound(
        id,
        Some(found.clone()),
    )));
    let saved = app.store.accounts().unwrap().remove(0);
    assert_eq!(saved.smtp.host, "smtp.orange.fr");
    assert_eq!(saved.smtp.username, "a@orange.fr", "the IMAP login");
    // Already configured: a late answer changes nothing.
    let other = ServerConfig {
        host: "smtp.other.fr".into(),
        ..found
    };
    let _ = app.update(Message::Account(AccountMsg::SmtpFound(id, Some(other))));
    assert_eq!(app.accounts[0].smtp.host, "smtp.orange.fr");
}

#[test]
fn reconnection_with_another_account_is_refused() {
    let mut app = app_with(|s| {
        add_account_with_mail(s, "jean@gmail.com", 1);
    });
    let id = app.accounts[0].id;
    let error = AccountError::new(ErrorKind::Reconnect, "Reconnexion nécessaire");
    let _ = app.update(Message::Worker(id, Event::Synced(Err(error))));
    app.overlay = Some(Overlay::Reconnect(id));
    let grant = crate::auth::OAuthGrant {
        provider: Provider::Google,
        email: Some("autre@gmail.com".into()),
        refresh_token: Some("1//rt".into()),
        access: crate::auth::AccessToken {
            token: "ya29".into(),
            expires_at: 0,
        },
    };
    let _ = app.update(Message::Account(AccountMsg::Reconnected(id, Ok(grant))));
    let toast = app.toast.as_ref().unwrap();
    assert!(toast.error && toast.text.contains("autre@gmail.com"));
    assert!(
        app.status[&id].error.is_some(),
        "still needs a reconnection"
    );
}

#[test]
fn signing_in_to_an_existing_address_moves_it_to_oauth() {
    use crate::account_form::Stage;
    let mut app = app_with(|s| {
        add_account_with_mail(s, "Jean@gmail.com", 1);
    });
    let id = app.accounts[0].id;
    let _ = app.update(Message::Add(AddMsg::Open(None)));
    let generation = match &mut app.overlay {
        Some(Overlay::AddAccount(form)) => {
            form.stage = Stage::Authorizing(Provider::Google);
            form.generation
        }
        other => panic!("no form: {other:?}"),
    };
    let grant = crate::auth::OAuthGrant {
        provider: Provider::Google,
        email: Some("jean@gmail.com".into()),
        refresh_token: Some("1//rt".into()),
        access: crate::auth::AccessToken {
            token: "ya29".into(),
            expires_at: 0,
        },
    };
    let _ = app.update(Message::Add(AddMsg::SignedIn(generation, Ok(grant))));
    assert!(app.overlay.is_none(), "the dialog closes");
    let accounts = app.store.accounts().unwrap();
    assert_eq!(accounts.len(), 1, "no duplicate");
    assert_eq!(accounts[0].id, id);
    assert_eq!(accounts[0].auth, avero_core::AuthKind::OAuthGoogle);
    assert_eq!(accounts[0].imap.host, "imap.gmail.com");
    assert_eq!(app.accounts[0].auth, avero_core::AuthKind::OAuthGoogle);
    assert!(!app.toast.as_ref().unwrap().error);
}

#[test]
fn debug_output_never_contains_secrets() {
    let secret = "Tr3s-Secret-Pw";
    let dialog = PasswordDialog {
        account: AccountId(1),
        password: secret.into(),
        error: None,
        checking: false,
        generation: 0,
    };
    let mut form = AccountForm::new();
    form.set(Field::Password, secret.into());
    let texts = [
        format!("{dialog:?}"),
        format!("{:?}", Overlay::Password(dialog.clone())),
        format!("{:?}", Overlay::AddAccount(Box::new(form))),
        format!(
            "{:?}",
            Message::Add(AddMsg::Field(Field::Password, secret.into()))
        ),
        format!(
            "{:?}",
            Message::Account(AccountMsg::PasswordText(secret.into()))
        ),
    ];
    for text in texts {
        assert!(!text.contains(secret), "{text}");
    }
    // Other fields are still readable.
    let email = format!("{:?}", AddMsg::Field(Field::Email, "a@x.fr".into()));
    assert!(email.contains("a@x.fr"));
}

/// The real add-account path with a keyring in memory: the password ends
/// up in the secret store and nowhere in the database file.
#[test]
fn password_never_written_to_database() {
    use avero_core::secrets::{MemorySecrets, SecretStore};
    let password = "Pa55-never-on-disk-91827";
    let dir = std::env::temp_dir().join(format!("avero-nopw-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("avero.db");
    let secrets = Arc::new(MemorySecrets::new());
    let backend = Backend::Live {
        db_path: db_path.clone(),
        secrets: secrets.clone(),
        config_dir: dir.clone(),
        workers: HashMap::new(),
    };
    let store = Store::open(&db_path).unwrap();
    let (mut app, _) = App::new(store, backend, Settings::default(), None);

    let _ = app.update(Message::Add(AddMsg::Open(None)));
    let _ = app.update(Message::Add(AddMsg::Field(
        Field::Email,
        "jean@orange.fr".into(),
    )));
    let _ = app.update(Message::Add(AddMsg::SubmitEmail));
    let generation = form(&app).generation;
    let config = avero_autoconfig::known("jean@orange.fr");
    let _ = app.update(Message::Add(AddMsg::Detected(generation, config)));
    let _ = app.update(Message::Add(AddMsg::Field(
        Field::Password,
        password.into(),
    )));
    let _ = app.update(Message::Add(AddMsg::SubmitPassword));
    // The login test answers through a task; deliver a success by hand.
    let generation = form(&app).generation;
    let _ = app.update(Message::Add(AddMsg::Checked(generation, Ok(()))));
    assert_eq!(app.accounts.len(), 1);
    let id = app.accounts[0].id;
    assert_eq!(secrets.get(id).unwrap().as_deref(), Some(password));

    // Look at every byte written to disk (database, WAL, journal).
    drop(app);
    let needle = password.as_bytes();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap_or_default();
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "password found in {}",
            path.display()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn worker_failure_on_action_triggers_resync() {
    let mut app = two_accounts();
    let (a, _) = ids(&app);
    let error = AccountError::new(ErrorKind::Network, "perdu");
    let _ = app.update(Message::Worker(a, Event::Done(Err(error))));
    assert!(app.status[&a].syncing);
    assert!(app.toast.as_ref().unwrap().error);
}

#[test]
fn shortcuts_do_not_disturb_composer_or_dialogs() {
    let mut app = app_with(|s| {
        crate::demo::seed(s);
    });
    let _ = app.update(Message::MessageSelected(0));
    let _ = app.update(Message::Compose(ComposeMsg::New));
    let ctrl_r = Message::Key(
        keyboard::Key::Character("r".into()),
        keyboard::Modifiers::COMMAND,
    );
    let _ = app.update(ctrl_r);
    assert_eq!(app.composer.as_ref().unwrap().subject, "", "draft kept");
    let _ = app.update(Message::Compose(ComposeMsg::Cancel));
    let _ = app.update(Message::Add(AddMsg::Open(None)));
    let key_j = Message::Key(
        keyboard::Key::Character("j".into()),
        keyboard::Modifiers::default(),
    );
    let before = app.selected_index();
    let _ = app.update(key_j);
    assert_eq!(app.selected_index(), before);
}

/// Accounts merged by the database migration lose their secrets, the
/// kept account's secret stays.
#[test]
fn merged_accounts_secrets_are_erased() {
    use avero_core::secrets::{MemorySecrets, SecretStore};
    let path = std::env::temp_dir().join(format!("avero-merged-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let store = Store::open(&path).unwrap();
        add_account_with_mail(&store, "a@x.fr", 1);
    }
    {
        // Rebuild what an old version could leave: a second "A@x.fr",
        // in a version 3 database (without what versions 4 to 7 added).
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE senders;
             DROP INDEX messages_sender;
             DROP INDEX messages_flagged;
             ALTER TABLE messages DROP COLUMN flagged;
             ALTER TABLE messages DROP COLUMN flag_pending;
             ALTER TABLE messages DROP COLUMN category;
             ALTER TABLE accounts DROP COLUMN signature;
             DROP INDEX messages_thread;
             DROP TABLE thread_links;
             ALTER TABLE messages DROP COLUMN in_reply_to;
             ALTER TABLE messages DROP COLUMN refs;
             ALTER TABLE messages DROP COLUMN to_addrs;
             ALTER TABLE messages DROP COLUMN delivered_to;
             ALTER TABLE messages DROP COLUMN list_unsubscribe;
             ALTER TABLE messages DROP COLUMN list_unsubscribe_post;
             ALTER TABLE messages DROP COLUMN list_id;
             ALTER TABLE messages DROP COLUMN precedence;
             ALTER TABLE messages DROP COLUMN auto_submitted;
             ALTER TABLE messages DROP COLUMN headers_v2;
             ALTER TABLE messages DROP COLUMN thread_id;
             DROP INDEX accounts_email_unique;
             INSERT INTO accounts (id, name, email, color_r, color_g, color_b,
                                   imap_host, imap_port, imap_user)
             VALUES (2, 'A', 'A@x.fr', 1, 2, 3, 'imap.x.fr', 993, 'A@x.fr');
             PRAGMA user_version = 3;",
        )
        .unwrap();
    }
    let secrets = MemorySecrets::new();
    secrets.set(AccountId(1), "keep").unwrap();
    secrets.set(AccountId(2), "forget").unwrap();

    let store = Store::open(&path).unwrap();
    forget_merged_secrets(&store, &secrets);

    assert_eq!(secrets.get(AccountId(1)).unwrap().as_deref(), Some("keep"));
    assert_eq!(secrets.get(AccountId(2)).unwrap(), None);
    drop(store);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn background_sync_updates_status_but_not_the_spinner() {
    let mut app = two_accounts();
    let (a, _) = ids(&app);
    let _ = app.update(Message::Refresh);
    let error = AccountError::new(ErrorKind::Network, "coupé");
    let _ = app.update(Message::Background(a, Event::Synced(Err(error.clone()))));
    assert_eq!(app.status[&a].error.as_ref(), Some(&error));
    // The user's own sync is still running.
    assert!(app.status[&a].syncing);
    let _ = app.update(Message::Background(a, Event::Synced(Ok(true))));
    assert!(app.status[&a].error.is_none());
}

#[test]
fn new_mail_notices_follow_the_setting() {
    let mut app = two_accounts();
    let (a, _) = ids(&app);
    let arrival = avero_sync::sync::Arrival {
        uid: 9,
        from_name: Some("Alice".to_string()),
        from_addr: "alice@x.fr".to_string(),
        subject: "Bonjour".to_string(),
    };
    let notices = app.notices(a, std::slice::from_ref(&arrival));
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].title, "Alice");
    assert_eq!(notices[0].body, "Bonjour");
    let _ = app.update(Message::NotificationsToggled(false));
    assert!(!app.settings.notifications);
    assert!(app.notices(a, &[arrival]).is_empty());
}

#[test]
fn events_of_a_removed_account_are_ignored() {
    let mut app = two_accounts();
    let gone = AccountId(9999);
    let _ = app.update(Message::Background(gone, Event::Synced(Ok(true))));
    assert!(!app.status.contains_key(&gone));
}

/// One account whose inbox holds "Sujet 1" to "Sujet 3", with a
/// conversation on "Sujet 1": the user's answer in Sent (read), then an
/// unread reply in the inbox, the newest message.
fn with_conversation() -> App {
    app_with(|s| {
        let a = add_account_with_mail(s, "a@x.fr", 3);
        let inbox = s.folder_by_role(a, FolderRole::Inbox).unwrap().unwrap();
        let sent = s.upsert_folder(a, "Sent", FolderRole::Sent).unwrap();
        let root = "<1@a@x.fr>".to_string();
        let mut answer = NewMessage {
            uid: 1,
            message_id: Some("<answer@a>".into()),
            subject: "Re: Sujet 1".into(),
            from_addr: "a@x.fr".into(),
            date: 1_800_000_000,
            seen: true,
            ..Default::default()
        };
        answer.headers.in_reply_to = Some(root.clone());
        answer.headers.references = vec![root.clone()];
        s.insert_messages(sent.id, &[answer]).unwrap();
        let mut back = NewMessage {
            uid: 10,
            message_id: Some("<back@x>".into()),
            subject: "Re: Sujet 1".into(),
            from_addr: "x@y.z".into(),
            date: 1_800_000_100,
            ..Default::default()
        };
        back.headers.in_reply_to = Some("<answer@a>".into());
        back.headers.references = vec![root, "<answer@a>".into()];
        s.insert_messages(inbox.id, &[back]).unwrap();
    })
}

/// A cached raw message for `row`, so that the reader can parse it.
fn cache_body(app: &App, row: i64, subject: &str) {
    let raw = format!(
        "From: x@y.z\r\nTo: a@x.fr\r\nSubject: {subject}\r\nMessage-ID: <{row}@cached>\r\n\r\nTexte {row}\r\n"
    );
    app.store
        .put_body(row, raw.as_bytes(), &format!("Texte {row}"), false)
        .unwrap();
}

/// Delivers the parsed body of reader entry `index`, as the background
/// parsing task would.
fn parse_entry(app: &mut App, index: usize) {
    let row = app.reading.as_ref().unwrap().entries[index].item.row_ids[0];
    let (raw, _) = app.store.body(row).unwrap().expect("a cached body");
    let opened = crate::background::OpenedBody::parse(&raw);
    let _ = app.update(Message::BodyParsed(row, Ok(Arc::new(opened))));
}

#[test]
fn a_conversation_is_one_line_with_its_count_and_senders() {
    let app = with_conversation();
    let a = app.accounts[0].id;
    assert_eq!(app.items.len(), 3);
    let line = &app.items[0];
    assert_eq!(line.subject, "Re: Sujet 1");
    assert_eq!(line.count, 3);
    assert_eq!(line.row_ids.len(), 2, "the two messages in the inbox");
    assert!(!line.seen);
    assert_eq!(line.senders, vec!["x@y.z", "moi"]);
    assert_eq!(crate::view::list::sender_line(line), "x, moi");
    // One unread conversation and two unread messages.
    assert_eq!(app.unread[&a], 3);
}

#[test]
fn sender_line_shortens_long_conversations() {
    let mut item = with_conversation().items[0].clone();
    item.senders = vec![
        "Marie Durand".into(),
        "moi".into(),
        "paul@ex.fr".into(),
        "Zoé Martin".into(),
    ];
    assert_eq!(crate::view::list::sender_line(&item), "Marie … paul, Zoé");
    item.senders.truncate(1);
    assert_eq!(crate::view::list::sender_line(&item), "Marie Durand");
    item.thread_id = None;
    assert_eq!(crate::view::list::sender_line(&item), "Marie Durand");
}

#[test]
fn opening_a_conversation_unfolds_the_newest_message() {
    let mut app = with_conversation();
    let rows = app.items[0].row_ids.clone();
    let _ = app.update(Message::MessageSelected(0));
    let reading = app.reading.as_ref().unwrap();
    assert!(reading.is_thread());
    let subjects: Vec<&str> = reading
        .entries
        .iter()
        .map(|e| e.item.subject.as_str())
        .collect();
    assert_eq!(subjects, vec!["Sujet 1", "Re: Sujet 1", "Re: Sujet 1"]);
    assert_eq!(reading.focus, 2);
    let unfolded: Vec<bool> = reading.entries.iter().map(|e| e.expanded).collect();
    assert_eq!(unfolded, vec![false, false, true]);
    assert!(matches!(reading.entries[0].body, Body::NotLoaded));
    assert!(matches!(reading.body(), Body::Loading));
    // Every message of the conversation in the inbox is now read.
    assert!(app.items[0].seen);
    assert_eq!(app.items[0].row_ids, rows);
    assert_eq!(app.selected_index(), Some(0));
}

#[test]
fn unfolding_loads_on_demand_and_moves_the_reply_target() {
    let mut app = with_conversation();
    let _ = app.update(Message::MessageSelected(0));
    let first = app.reading.as_ref().unwrap().entries[0].item.row_ids[0];
    cache_body(&app, first, "Sujet 1");

    let _ = app.update(Message::EntryToggled(0));
    let reading = app.reading.as_ref().unwrap();
    assert!(reading.entries[0].expanded);
    assert_eq!(reading.focus, 0);
    assert!(matches!(reading.entries[0].body, Body::Loading));
    parse_entry(&mut app, 0);
    assert!(matches!(
        app.reading.as_ref().unwrap().body(),
        Body::Ready { .. }
    ));

    // Reply answers the message unfolded last.
    let _ = app.update(Message::Answer(Mode::Reply));
    let composer = app.composer.as_ref().unwrap();
    assert_eq!(composer.subject, "Re: Sujet 1");
    assert_eq!(
        composer.in_reply_to.as_deref(),
        Some(format!("<{first}@cached>").as_str())
    );
    let _ = app.update(Message::Compose(ComposeMsg::Cancel));

    // Folding it back: the newest one is the target again.
    let _ = app.update(Message::EntryToggled(0));
    let reading = app.reading.as_ref().unwrap();
    assert!(!reading.entries[0].expanded);
    assert_eq!(reading.focus, 2);
    // The last unfolded message stays unfolded.
    let _ = app.update(Message::EntryToggled(2));
    assert!(app.reading.as_ref().unwrap().entries[2].expanded);
}

#[test]
fn delete_and_mark_unread_act_on_the_whole_conversation_in_the_folder() {
    let mut app = with_conversation();
    synced(&mut app, true);
    let a = app.accounts[0].id;
    let rows = app.items[0].row_ids.clone();
    let _ = app.update(Message::MessageSelected(0));
    assert_eq!(app.unread[&a], 2);
    let _ = app.update(Message::MarkUnread);
    assert!(!app.items[0].seen);
    assert_eq!(app.unread[&a], 3);

    let _ = app.update(Message::MessageSelected(0));
    let _ = app.update(Message::Delete);
    assert_eq!(app.items.len(), 2);
    assert!(app.items.iter().all(|i| i.row_ids != rows));
    let _ = app.update(Message::Moved(rows.clone(), Event::Done(Ok(()))));
    assert!(app.store.message_refs(&rows).unwrap().is_empty());
    // The answer in Sent stays where it is.
    let _ = app.update(Message::FolderSelected(FolderRole::Sent));
    assert_eq!(app.items.len(), 1);
}

#[test]
fn turning_conversations_off_lists_every_message() {
    let mut app = with_conversation();
    let a = app.accounts[0].id;
    let _ = app.update(Message::ThreadsToggled(false));
    assert!(!app.settings.group_threads);
    assert_eq!(app.items.len(), 4);
    assert!(
        app.items
            .iter()
            .all(|i| i.thread_id.is_none() && i.count == 1)
    );
    assert_eq!(app.unread[&a], 4);
    let _ = app.update(Message::MessageSelected(0));
    let reading = app.reading.as_ref().unwrap();
    assert!(!reading.is_thread());
    assert_eq!(reading.entries.len(), 1);

    let _ = app.update(Message::ThreadsToggled(true));
    assert_eq!(app.items.len(), 3);
    assert!(app.reading.is_none(), "the list starts again");
}

#[test]
fn search_stays_one_line_per_message() {
    let mut app = with_conversation();
    let _ = app.update(Message::SearchChanged("Sujet".into()));
    let _ = app.update(Message::SearchSubmitted);
    // The three inbox subjects, the reply and the answer in Sent.
    assert_eq!(app.items.len(), 5);
    let _ = app.update(Message::MessageSelected(0));
    assert!(!app.reading.as_ref().unwrap().is_thread());
}

#[test]
fn a_reply_arriving_keeps_the_conversation_selected() {
    let mut app = with_conversation();
    let a = app.accounts[0].id;
    let _ = app.update(Message::MessageSelected(0));
    let inbox = app
        .store
        .folder_by_role(a, FolderRole::Inbox)
        .unwrap()
        .unwrap();
    let mut late = NewMessage {
        uid: 11,
        message_id: Some("<late@x>".into()),
        subject: "Re: Sujet 1".into(),
        from_addr: "x@y.z".into(),
        date: 1_800_000_200,
        ..Default::default()
    };
    late.headers.in_reply_to = Some("<back@x>".into());
    app.store.insert_messages(inbox.id, &[late]).unwrap();
    let _ = app.update(Message::Worker(a, Event::Progress));
    assert_eq!(app.selected_index(), Some(0));
    assert_eq!(app.items[0].row_ids.len(), 3);
    assert_eq!(app.selected.as_ref(), Some(&app.items[0].row_ids));
}

#[test]
fn demo_conversation_can_be_drawn_unfolded() {
    let mut app = app_with(crate::demo::seed);
    let index = app
        .items
        .iter()
        .position(|i| i.subject.starts_with("Re: Point budget"))
        .unwrap();
    assert_eq!(app.items[index].count, 3);
    let _ = app.update(Message::MessageSelected(index));
    // The newest one is unfolded already.
    for entry in 0..3 {
        if entry < 2 {
            let _ = app.update(Message::EntryToggled(entry));
        }
        parse_entry(&mut app, entry);
    }
    let reading = app.reading.as_ref().unwrap();
    assert!(reading.entries.iter().all(|e| e.expanded));
    assert!(
        reading
            .entries
            .iter()
            .all(|e| matches!(e.body, Body::Ready { .. }))
    );
    let _ = app.view();
    // Settings and the per-message view draw too.
    let _ = app.update(Message::ShowSettings);
    let _ = app.view();
    let _ = app.update(Message::ThreadsToggled(false));
    let _ = app.update(Message::ShowMail);
    open_parsed(&mut app, 0);
    let _ = app.view();
}

// ------------------------------------------------------------ signatures

#[test]
fn signature_is_edited_in_the_settings() {
    let mut app = two_accounts();
    let (a, _) = ids(&app);
    let _ = app.update(Message::Account(AccountMsg::EditSignature(a)));
    let Some(Overlay::Signature { content, .. }) = &mut app.overlay else {
        panic!("signature dialog expected");
    };
    *content = text_editor::Content::with_text("Marie\nPâtisserie Durand\n\n");
    let _ = app.view();
    let _ = app.update(Message::Account(AccountMsg::SaveSignature));
    assert!(app.overlay.is_none());
    assert_eq!(app.accounts[0].signature, "Marie\nPâtisserie Durand");
    assert_eq!(
        app.store.accounts().unwrap()[0].signature,
        "Marie\nPâtisserie Durand"
    );
}

#[test]
fn new_message_is_signed_and_follows_the_sender() {
    let mut app = app_with(|s| {
        let a = add_account_with_mail(s, "a@x.fr", 1);
        let b = add_account_with_mail(s, "b@x.fr", 1);
        s.set_signature(a, "Signé A").unwrap();
        s.set_signature(b, "Signé B").unwrap();
    });
    let (a, b) = ids(&app);
    let _ = app.update(Message::Compose(ComposeMsg::New));
    let body = |app: &App| app.composer.as_ref().unwrap().body.text();
    assert_eq!(app.composer.as_ref().unwrap().from, a);
    assert_eq!(body(&app), "\n\n-- \nSigné A");
    let _ = app.update(Message::Compose(ComposeMsg::From(b)));
    assert_eq!(body(&app), "\n\n-- \nSigné B");
}

#[test]
fn reply_and_forward_are_signed_above_the_quote() {
    let mut app = app_with(|s| {
        crate::demo::seed(s);
        for account in s.accounts().unwrap() {
            s.set_signature(account.id, "Camille").unwrap();
        }
    });
    open_parsed(&mut app, 0);
    for mode in [Mode::Reply, Mode::Forward] {
        let _ = app.update(Message::Answer(mode));
        let body = app.composer.as_ref().unwrap().body.text();
        assert!(body.starts_with("\n\n-- \nCamille\n\n"), "{body:?}");
        assert!(body.len() > "\n\n-- \nCamille\n\n".len(), "{body:?}");
        let _ = app.update(Message::Compose(ComposeMsg::Cancel));
    }
}

// ------------------------------------------------------------- shortcuts

fn press(app: &mut App, key: &str, at: Instant) {
    let _ = app.on_key_at(
        keyboard::Key::Character(key.into()),
        keyboard::Modifiers::default(),
        at,
    );
}

fn press_named(app: &mut App, key: keyboard::key::Named) {
    let _ = app.update(Message::Key(
        keyboard::Key::Named(key),
        keyboard::Modifiers::default(),
    ));
}

#[test]
fn g_then_a_letter_goes_to_a_folder() {
    let mut app = two_accounts();
    let start = Instant::now();
    for (letter, folder) in [
        ("s", FolderRole::Sent),
        ("d", FolderRole::Drafts),
        ("a", FolderRole::Archive),
        ("t", FolderRole::Trash),
        ("i", FolderRole::Inbox),
    ] {
        press(&mut app, "g", start);
        press(&mut app, letter, start + Duration::from_millis(800));
        assert_eq!(app.folder, folder, "g {letter}");
    }
    // The letter alone, or `g` then another key, does nothing.
    press(&mut app, "s", start);
    assert_eq!(app.folder, FolderRole::Inbox);
    press(&mut app, "g", start);
    press(&mut app, "z", start);
    press(&mut app, "s", start);
    assert_eq!(app.folder, FolderRole::Inbox);
}

#[test]
fn g_waits_one_and_a_half_seconds() {
    let mut app = two_accounts();
    let start = Instant::now();
    press(&mut app, "g", start);
    press(&mut app, "s", start + Duration::from_millis(1600));
    assert_eq!(app.folder, FolderRole::Inbox, "too late");
    assert!(app.pending_g.is_none());
    press(&mut app, "g", start);
    press(&mut app, "t", start + Duration::from_millis(1400));
    assert_eq!(app.folder, FolderRole::Trash);
}

#[test]
fn question_mark_shows_the_shortcuts_until_escape() {
    let mut app = two_accounts();
    press(&mut app, "?", Instant::now());
    assert!(matches!(app.overlay, Some(Overlay::Shortcuts)));
    let _ = app.view();
    // Shortcuts are off under a dialog.
    press(&mut app, "j", Instant::now());
    assert_eq!(app.selected, None);
    press_named(&mut app, keyboard::key::Named::Escape);
    assert!(app.overlay.is_none());
}

#[test]
fn action_keys_act_on_the_line_read() {
    let mut app = two_accounts();
    synced(&mut app, true);
    let (a, b) = ids(&app);
    let now = Instant::now();
    press_named(&mut app, keyboard::key::Named::ArrowDown);
    assert_eq!(app.selected_index(), Some(0));
    assert_eq!(app.unread[&a] + app.unread[&b], 4);
    press(&mut app, "u", now);
    assert_eq!(app.unread[&a] + app.unread[&b], 5);
    // Archive (e), then delete (#, as AltGr+3 types it).
    press(&mut app, "e", now);
    assert_eq!(app.items.len(), 4);
    let _ = app.on_key_at(
        keyboard::Key::Character("#".into()),
        keyboard::Modifiers::CTRL | keyboard::Modifiers::ALT,
        now,
    );
    assert_eq!(app.items.len(), 3);
    press_named(&mut app, keyboard::key::Named::Delete);
    assert_eq!(app.items.len(), 2);
    // c writes a new message; then the one-key shortcuts are off.
    press(&mut app, "c", now);
    assert!(app.composer.is_some());
    press(&mut app, "e", now);
    press(&mut app, "j", now);
    assert_eq!(app.items.len(), 2);
    assert!(app.composer.is_some());
}

#[test]
fn enter_opens_and_reply_keys_answer() {
    let mut app = app_with(|s| {
        crate::demo::seed(s);
    });
    open_parsed(&mut app, 0);
    press(&mut app, "x", Instant::now());
    assert!(app.reading.is_none());
    press_named(&mut app, keyboard::key::Named::Enter);
    assert!(app.checked.is_empty());
    assert!(app.reading.is_some());
    // Enter loads the body again: deliver it.
    let row = app.reading.as_ref().unwrap().focused().item.row_ids[0];
    let (raw, _) = app.store.body(row).unwrap().unwrap();
    let opened = crate::background::OpenedBody::parse(&raw);
    let _ = app.update(Message::BodyParsed(row, Ok(Arc::new(opened))));
    press(&mut app, "f", Instant::now());
    assert!(app.composer.as_ref().unwrap().subject.starts_with("Fwd: "));
}

// ------------------------------------------------------------- selection

fn click(app: &mut App, index: usize, modifiers: keyboard::Modifiers) {
    let _ = app.update(Message::ModifiersChanged(modifiers));
    let _ = app.update(Message::MessageSelected(index));
    let _ = app.update(Message::ModifiersChanged(keyboard::Modifiers::default()));
}

fn checked_indexes(app: &App) -> Vec<usize> {
    (0..app.items.len())
        .filter(|&i| app.is_checked(&app.items[i]))
        .collect()
}

#[test]
fn ctrl_click_adds_and_removes_lines() {
    let mut app = two_accounts();
    click(&mut app, 0, keyboard::Modifiers::COMMAND);
    click(&mut app, 2, keyboard::Modifiers::COMMAND);
    assert_eq!(checked_indexes(&app), vec![0, 2]);
    assert!(app.reading.is_none());
    let _ = app.view();
    click(&mut app, 0, keyboard::Modifiers::COMMAND);
    assert_eq!(checked_indexes(&app), vec![2]);
    // A plain click opens a line and forgets the others.
    click(&mut app, 1, keyboard::Modifiers::default());
    assert!(app.checked.is_empty());
    assert!(app.reading.is_some());
    // The line being read joins the first Ctrl+click.
    click(&mut app, 3, keyboard::Modifiers::COMMAND);
    assert_eq!(checked_indexes(&app), vec![1, 3]);
}

#[test]
fn shift_click_takes_a_range() {
    let mut app = two_accounts();
    click(&mut app, 1, keyboard::Modifiers::default());
    click(&mut app, 3, keyboard::Modifiers::SHIFT);
    assert_eq!(checked_indexes(&app), vec![1, 2, 3]);
    // From the same anchor, the other way.
    click(&mut app, 0, keyboard::Modifiers::SHIFT);
    assert_eq!(checked_indexes(&app), vec![0, 1]);
}

#[test]
fn x_toggles_the_keyboard_line() {
    let mut app = two_accounts();
    let now = Instant::now();
    press(&mut app, "j", now);
    press(&mut app, "x", now);
    assert_eq!(checked_indexes(&app), vec![0]);
    // J moves without opening, X takes the next line too.
    press(&mut app, "j", now);
    assert!(app.reading.is_none());
    assert_eq!(app.selected_index(), Some(1));
    press(&mut app, "x", now);
    assert_eq!(checked_indexes(&app), vec![0, 1]);
    press(&mut app, "x", now);
    assert_eq!(checked_indexes(&app), vec![0]);
    press_named(&mut app, keyboard::key::Named::Escape);
    assert!(app.checked.is_empty());
}

#[test]
fn select_all_takes_the_loaded_lines() {
    let mut app = two_accounts();
    let _ = app.update(Message::SelectAll(true));
    assert_eq!(checked_indexes(&app), vec![0, 1, 2, 3, 4]);
    assert!(app.all_checked());
    let _ = app.update(Message::SelectAll(false));
    assert!(app.checked.is_empty());
    // Changing folder forgets the selection.
    let _ = app.update(Message::SelectAll(true));
    let _ = app.update(Message::FolderSelected(FolderRole::Sent));
    assert!(app.checked.is_empty());
}

#[test]
fn grouped_actions_span_accounts() {
    let mut app = two_accounts();
    synced(&mut app, true);
    let (a, b) = ids(&app);
    let _ = app.update(Message::SelectAll(true));
    assert_eq!(app.action_accounts(), vec![a, b]);
    // One job per account.
    let refs = app.store.message_refs(&app.action_rows()).unwrap();
    let groups = super::mail::by_account(refs);
    assert_eq!(groups.len(), 2);
    assert_eq!((groups[&a].len(), groups[&b].len()), (3, 2));

    let _ = app.update(Message::MarkRead);
    assert_eq!((app.unread[&a], app.unread[&b]), (0, 0));
    assert_eq!(app.checked.len(), 5, "still selected");
    let _ = app.update(Message::MarkUnread);
    assert_eq!((app.unread[&a], app.unread[&b]), (3, 2));

    // Archive: everything hidden at once; b's server refuses.
    let rows_a: Vec<i64> = groups[&a].iter().map(|r| r.row_id).collect();
    let rows_b: Vec<i64> = groups[&b].iter().map(|r| r.row_id).collect();
    let _ = app.update(Message::Archive);
    assert!(app.items.is_empty());
    assert!(app.checked.is_empty());
    let _ = app.update(Message::Moved(rows_a, Event::Done(Ok(()))));
    let error = AccountError::new(ErrorKind::Network, "Connexion impossible");
    let _ = app.update(Message::Moved(rows_b, Event::Done(Err(error))));
    assert_eq!(app.items.len(), 2);
    assert!(app.items.iter().all(|i| i.accounts == vec![b]));
}

#[test]
fn grouped_delete_with_the_delete_key() {
    let mut app = two_accounts();
    synced(&mut app, true);
    click(&mut app, 0, keyboard::Modifiers::COMMAND);
    click(&mut app, 4, keyboard::Modifiers::COMMAND);
    press_named(&mut app, keyboard::key::Named::Delete);
    assert_eq!(app.items.len(), 3);
    assert!(app.checked.is_empty());
}

// ------------------------------------------- tabs, favourites, senders

/// Two accounts; a's inbox also holds a newsletter (unsubscribe by mail),
/// a promotion (one-click) and a notification, b's the same newsletter.
fn with_lists() -> App {
    app_with(|s| {
        let a = add_account_with_mail(s, "a@x.fr", 2);
        let b = add_account_with_mail(s, "b@x.fr", 1);
        let list = |uid: u32, from: &str, date: i64| {
            let mut m = NewMessage {
                uid,
                message_id: Some(format!("<{uid}.{from}>")),
                subject: format!("Lettre {uid}"),
                from_name: Some("La Lettre".into()),
                from_addr: from.into(),
                date,
                ..Default::default()
            };
            m.headers.list_unsubscribe =
                Some("<mailto:stop@news.fr?subject=STOP>, <https://news.fr/u?id=1>".into());
            m
        };
        let inbox_a = s.folder_by_role(a, FolderRole::Inbox).unwrap().unwrap();
        let mut one_click = list(20, "promo@shop.fr", 1_700_000_050);
        one_click.headers.list_unsubscribe_post = Some("List-Unsubscribe=One-Click".into());
        let notification = NewMessage {
            uid: 30,
            message_id: Some("<30@robot>".into()),
            subject: "Alerte".into(),
            from_addr: "noreply@robot.fr".into(),
            date: 1_700_000_060,
            seen: true,
            ..Default::default()
        };
        s.insert_messages(
            inbox_a.id,
            &[
                list(10, "lettre@news.fr", 1_700_000_040),
                one_click,
                notification,
            ],
        )
        .unwrap();
        let inbox_b = s.folder_by_role(b, FolderRole::Inbox).unwrap().unwrap();
        s.insert_messages(inbox_b.id, &[list(11, "lettre@news.fr", 1_700_000_070)])
            .unwrap();
    })
}

fn subjects(app: &App) -> Vec<String> {
    app.items.iter().map(|i| i.subject.clone()).collect()
}

fn line_of(app: &App, subject: &str) -> usize {
    subjects(app).iter().position(|s| s == subject).unwrap()
}

fn offer(app: &App) -> Option<&UnsubscribeOffer> {
    app.reading.as_ref().unwrap().unsubscribe.as_ref()
}

#[test]
fn inbox_tabs_split_the_inbox_and_count_unread() {
    let mut app = with_lists();
    assert!(app.tabs_shown());
    assert_eq!(app.tab, Category::Important);
    assert_eq!(app.items.len(), 3, "{:?}", subjects(&app));
    assert_eq!(app.tab_unread[&Category::Important], 3);
    assert_eq!(app.tab_unread[&Category::Newsletter], 3);
    assert_eq!(app.tab_unread[&Category::Notification], 0);

    let _ = app.update(Message::TabSelected(Category::Newsletter));
    assert_eq!(subjects(&app), vec!["Lettre 11", "Lettre 20", "Lettre 10"]);
    // The account filter applies to the tabs and their counts.
    let (a, _) = ids(&app);
    let _ = app.update(Message::FilterSelected(AccountFilter::Only(a)));
    assert_eq!(app.items.len(), 2);
    assert_eq!(app.tab_unread[&Category::Newsletter], 2);
    let _ = app.update(Message::TabSelected(Category::Notification));
    assert_eq!(subjects(&app), vec!["Alerte"]);

    // Another folder has no tabs; coming back starts on Important.
    let _ = app.update(Message::FolderSelected(FolderRole::Sent));
    assert!(!app.tabs_shown());
    let _ = app.update(Message::FolderSelected(FolderRole::Inbox));
    assert_eq!(app.tab, Category::Important);

    // Turned off: one list again.
    let _ = app.update(Message::FilterSelected(AccountFilter::All));
    let _ = app.update(Message::TabsToggled(false));
    assert!(!app.tabs_shown());
    assert_eq!(app.items.len(), 7);
}

#[test]
fn reclassifying_a_conversation_remembers_the_sender() {
    let mut app = with_lists();
    let _ = app.update(Message::TabSelected(Category::Newsletter));
    let _ = app.update(Message::MessageSelected(line_of(&app, "Lettre 10")));
    let reading = app.reading.as_ref().unwrap();
    assert_eq!(reading.sender, "lettre@news.fr");
    assert_eq!(reading.category, Some(Category::Newsletter));

    let _ = app.update(Message::Reclassify(Category::Important));
    // Both mails of that sender left the tab (b's too); the other stays.
    assert_eq!(subjects(&app), vec!["Lettre 20"]);
    let _ = app.update(Message::TabSelected(Category::Important));
    assert!(subjects(&app).contains(&"Lettre 11".to_string()));
    // Remembered for the next mail of that sender.
    let (a, _) = ids(&app);
    let inbox = app
        .store
        .folder_by_role(a, FolderRole::Inbox)
        .unwrap()
        .unwrap();
    let mut next = NewMessage {
        uid: 99,
        message_id: Some("<99@news>".into()),
        subject: "Lettre 99".into(),
        from_addr: "Lettre@News.fr".into(),
        date: 1_700_000_100,
        ..Default::default()
    };
    next.headers.list_id = Some("<news.fr>".into());
    app.store.insert_messages(inbox.id, &[next]).unwrap();
    app.reload();
    assert_eq!(app.items[0].subject, "Lettre 99");
}

#[test]
fn sender_view_lists_every_account_then_goes_back() {
    let mut app = with_lists();
    let _ = app.update(Message::TabSelected(Category::Newsletter));
    let _ = app.update(Message::ShowSender(
        "LETTRE@news.fr".into(),
        "La Lettre".into(),
    ));
    assert!(!app.tabs_shown());
    assert!(!app.lines_are_threads());
    assert_eq!(subjects(&app), vec!["Lettre 11", "Lettre 10"]);
    let accounts: Vec<AccountId> = app.items.iter().flat_map(|i| i.accounts.clone()).collect();
    assert_eq!(accounts.len(), 2, "both accounts");
    // Échap (or "Retour") goes back to the tab that was shown.
    press_named(&mut app, keyboard::key::Named::Escape);
    assert!(app.sender.is_none());
    assert_eq!(app.tab, Category::Newsletter);
    assert_eq!(app.items.len(), 3);
    // Without a name, the address is the title; a search replaces the view.
    let _ = app.update(Message::ShowSender("lettre@news.fr".into(), String::new()));
    assert_eq!(app.sender.as_ref().unwrap().name, "lettre@news.fr");
    let _ = app.update(Message::SearchChanged("Alerte".into()));
    let _ = app.update(Message::SearchSubmitted);
    assert!(app.sender.is_none());
    assert_eq!(subjects(&app), vec!["Alerte"]);
}

#[test]
fn star_key_and_favourites() {
    let mut app = two_accounts();
    let _ = app.update(Message::MessageSelected(0));
    let start = Instant::now();
    press(&mut app, "s", start);
    assert!(app.items[0].flagged);
    // `g` then `s` still goes to Envoyés, without starring.
    press(&mut app, "g", start);
    press(&mut app, "s", start);
    assert_eq!(app.folder, FolderRole::Sent);

    let _ = app.update(Message::StarredSelected);
    assert!(app.starred);
    assert_eq!(app.items.len(), 1);
    assert!(app.items[0].flagged);
    // The star of the line takes it off: the line leaves "Favoris".
    let _ = app.update(Message::StarLine(0));
    assert!(app.items.is_empty());
    // Back in a folder, "Favoris" is left.
    let _ = app.update(Message::FolderSelected(FolderRole::Inbox));
    assert!(!app.starred);
    assert!(app.items.iter().all(|i| !i.flagged));
}

#[test]
fn starring_checked_lines_and_unstarring_them_all() {
    let mut app = two_accounts();
    click(&mut app, 0, keyboard::Modifiers::COMMAND);
    click(&mut app, 3, keyboard::Modifiers::COMMAND);
    let _ = app.update(Message::ToggleStar);
    let flagged: Vec<bool> = app.items.iter().map(|i| i.flagged).collect();
    assert_eq!(flagged, vec![true, false, false, true, false]);
    // All of them starred: the same action takes the stars off.
    let _ = app.update(Message::ToggleStar);
    assert!(app.items.iter().all(|i| !i.flagged));
}

#[test]
fn unsubscribe_asks_then_is_remembered_for_the_sender() {
    use crate::unsubscribe::Method;
    let mut app = with_lists();
    let _ = app.update(Message::TabSelected(Category::Newsletter));
    let _ = app.update(Message::MessageSelected(line_of(&app, "Lettre 10")));
    let found = offer(&app).unwrap().clone();
    // No one-click: the mail wins over the web page.
    assert!(matches!(
        &found.method,
        Method::Mail { to, subject, .. } if to == "stop@news.fr" && subject == "STOP"
    ));
    assert_eq!(found.account, ids(&app).0);
    assert!(!found.requested);

    let _ = app.update(Message::Unsubscribe);
    assert!(matches!(app.overlay, Some(Overlay::ConfirmUnsubscribe(_))));
    // Demo mode: nothing is sent, the request is remembered.
    let _ = app.update(Message::ConfirmUnsubscribe);
    assert!(app.overlay.is_none());
    assert!(offer(&app).unwrap().requested);
    // The other mail of that sender (b's account) knows it too.
    let _ = app.update(Message::MessageSelected(line_of(&app, "Lettre 11")));
    assert!(offer(&app).unwrap().requested);

    // One-click when offered over HTTPS.
    let _ = app.update(Message::MessageSelected(line_of(&app, "Lettre 20")));
    assert!(matches!(offer(&app).unwrap().method, Method::OneClick(_)));
    // A failure is reported and nothing is remembered.
    let _ = app.update(Message::Unsubscribed(
        "promo@shop.fr".into(),
        Err("refusé".into()),
    ));
    assert!(!offer(&app).unwrap().requested);
    assert!(app.toast.as_ref().unwrap().error);
    let _ = app.update(Message::Unsubscribed("promo@shop.fr".into(), Ok(())));
    assert!(offer(&app).unwrap().requested);
    // Plain mail has nothing to offer.
    let _ = app.update(Message::TabSelected(Category::Important));
    let _ = app.update(Message::MessageSelected(0));
    assert!(offer(&app).is_none());
}

#[test]
fn demo_has_tabs_favourites_and_unsubscribe() {
    let store = crate::demo::seeded_store();
    let (mut app, _task) = App::new(store, Backend::Demo, Settings::default(), None);
    for category in Category::ALL {
        assert!(app.tab_unread.contains_key(&category));
    }
    let _ = app.update(Message::TabSelected(Category::Newsletter));
    let monde = app
        .items
        .iter()
        .position(|i| i.from_addr == "newsletter@lemonde.fr")
        .unwrap();
    let _ = app.update(Message::MessageSelected(monde));
    assert!(matches!(
        offer(&app).unwrap().method,
        crate::unsubscribe::Method::OneClick(_)
    ));
    let _ = app.update(Message::StarredSelected);
    assert!(app.items.len() >= 3);
}
