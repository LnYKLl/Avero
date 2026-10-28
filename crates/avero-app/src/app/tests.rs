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
fn delete_is_disabled_in_trash() {
    let mut app = two_accounts();
    let _ = app.update(Message::FolderSelected(FolderRole::Trash));
    assert!(app.items.is_empty());
    assert!(!app.can_delete());
}

#[test]
fn archive_disabled_when_account_has_no_archive() {
    let mut app = two_accounts();
    let (a, _) = ids(&app);
    let _ = app.update(Message::FilterSelected(AccountFilter::Only(a)));
    let _ = app.update(Message::MessageSelected(0));
    assert!(app.can_archive(), "unknown counts as available");
    let _ = app.update(Message::Worker(a, Event::Synced(Ok(false))));
    assert!(!app.can_archive());
    let before = app.items.len();
    let _ = app.update(Message::Archive);
    assert_eq!(app.items.len(), before);
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
    let _ = app.update(Message::MessageSelected(index));
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
