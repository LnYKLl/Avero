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
    let row = app.items[index].row_ids[0];
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
    assert!(matches!(app.reading.as_ref().unwrap().body, Body::Loading));
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
    assert!(matches!(&app.reading.as_ref().unwrap().body, Body::Failed(e) if e == "hors ligne"));
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
        app.reading.as_ref().unwrap().body,
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
        &app.reading.as_ref().unwrap().body,
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
    assert!(matches!(app.reading.as_ref().unwrap().body, Body::Loading));
    let _ = app.update(Message::Answer(Mode::Reply));
    assert!(app.composer.is_none());
    open_parsed(&mut app, 0);
    assert!(matches!(
        app.reading.as_ref().unwrap().body,
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
    assert_eq!(form(&app).stage, crate::account_form::Stage::Credentials);
    assert!(form(&app).shows_password(), "no OAuth client in demo mode");
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
fn google_button_without_client_explains_why() {
    let mut app = app_with(|_| {});
    let _ = app.update(Message::Add(AddMsg::Open(Some(Provider::Google))));
    let note = form(&app).note.clone().unwrap();
    assert!(note.contains("docs/oauth-setup.md"), "{note}");
    assert_eq!(form(&app).stage, crate::account_form::Stage::Email);
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
