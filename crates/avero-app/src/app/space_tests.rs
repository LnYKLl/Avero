//! Spaces: what the active space limits, and how the settings change
//! them (demo mode, in-memory store).

use super::tests::{add_account_with_mail, app_with};
use super::*;
use crate::app::cleanup::CleanupMsg;

/// Three accounts (`a`: 3 unread, `b`: 2, `c`: 1); "Perso" holds `a` and
/// `b`, "Pro" holds `b` and `c` (`b` is in both).
fn three_accounts() -> App {
    app_with(|s| {
        let a = add_account_with_mail(s, "a@x.fr", 3);
        let b = add_account_with_mail(s, "b@x.fr", 2);
        let c = add_account_with_mail(s, "c@x.fr", 1);
        s.add_space("Perso", "house", 0, &[a, b]).unwrap();
        s.add_space("Pro", "briefcase", 1, &[b, c]).unwrap();
    })
}

fn ids3(app: &App) -> (AccountId, AccountId, AccountId) {
    (app.accounts[0].id, app.accounts[1].id, app.accounts[2].id)
}

fn space(app: &mut App, message: SpaceMsg) {
    let _ = app.update(Message::Space(message));
}

/// The accounts of the lines shown.
fn shown_accounts(app: &App) -> Vec<AccountId> {
    let mut accounts: Vec<AccountId> = app
        .items
        .iter()
        .flat_map(|item| item.accounts.clone())
        .collect();
    accounts.sort();
    accounts.dedup();
    accounts
}

#[test]
fn the_active_space_filters_the_list_and_the_counters() {
    let mut app = three_accounts();
    let (a, b, c) = ids3(&app);
    let perso = app.spaces[0].id;
    let pro = app.spaces[1].id;
    assert_eq!(app.items.len(), 6);
    assert_eq!(app.space_unread(None), 6);

    space(&mut app, SpaceMsg::Select(Some(perso)));
    assert_eq!(app.scope(), AccountFilter::Space(perso));
    assert_eq!(shown_accounts(&app), vec![a, b]);
    assert_eq!(app.space_unread(app.space), 5);
    let visible: Vec<AccountId> = app.space_accounts().iter().map(|a| a.id).collect();
    assert_eq!(visible, vec![a, b]);
    // The tabs count only the space's mail.
    assert_eq!(app.tab_unread[&Category::Important], 5);

    // `b` is in both spaces.
    space(&mut app, SpaceMsg::Select(Some(pro)));
    assert_eq!(shown_accounts(&app), vec![b, c]);
    assert_eq!(app.space_unread(app.space), 3);
    assert_eq!(app.space_unread(Some(perso)), 5);

    // "Tous" again.
    space(&mut app, SpaceMsg::Number(0));
    assert_eq!(app.space, None);
    assert_eq!(app.items.len(), 6);
}

#[test]
fn account_chips_work_inside_the_space() {
    let mut app = three_accounts();
    let (a, b, c) = ids3(&app);
    let perso = app.spaces[0].id;
    space(&mut app, SpaceMsg::Select(Some(perso)));
    let _ = app.update(Message::FilterSelected(AccountFilter::Only(b)));
    assert_eq!(app.scope(), AccountFilter::Only(b));
    assert_eq!(shown_accounts(&app), vec![b]);
    // `b` is also in "Pro": the chip stays.
    space(&mut app, SpaceMsg::Number(2));
    assert_eq!(app.filter, AccountFilter::Only(b));
    // `c` only in "Pro": going back to "Perso" lets the chip go.
    let _ = app.update(Message::FilterSelected(AccountFilter::Only(c)));
    space(&mut app, SpaceMsg::Number(1));
    assert_eq!(app.filter, AccountFilter::All);
    assert_eq!(shown_accounts(&app), vec![a, b]);
}

#[test]
fn search_favourites_and_new_mail_follow_the_space() {
    let mut app = three_accounts();
    let (a, b, _) = ids3(&app);
    let perso = app.spaces[0].id;
    space(&mut app, SpaceMsg::Select(Some(perso)));
    let _ = app.update(Message::SearchChanged("Sujet".into()));
    let _ = app.update(Message::SearchSubmitted);
    assert_eq!(app.items.len(), 5);
    assert_eq!(shown_accounts(&app), vec![a, b]);
    let _ = app.update(Message::SearchCleared);

    let all: Vec<i64> = app
        .store
        .list(
            &avero_core::Listing::Folder(FolderRole::Inbox),
            AccountFilter::All,
            false,
            50,
            0,
        )
        .unwrap()
        .into_iter()
        .flat_map(|i| i.row_ids)
        .collect();
    app.store.set_flagged(&all, true).unwrap();
    let _ = app.update(Message::StarredSelected);
    assert_eq!(shown_accounts(&app), vec![a, b]);
    // A new message is written from the space's first account.
    assert_eq!(app.default_sender(), Some(a));
}

#[test]
fn scheduled_mail_and_cleanup_follow_the_space() {
    let mut app = three_accounts();
    let (_, _, c) = ids3(&app);
    let perso = app.spaces[0].id;
    let fields = avero_core::DraftFields {
        account: c,
        to: "z@y.fr".into(),
        subject: "Plus tard".into(),
        message_id: "<s@x>".into(),
        ..Default::default()
    };
    let id = app.store.save_draft(None, &fields, Some(&[]), 1).unwrap();
    app.store.schedule_draft(id, i64::MAX / 2).unwrap();
    app.refresh_outbox();
    assert_eq!(app.outbox.len(), 1);
    space(&mut app, SpaceMsg::Select(Some(perso)));
    assert!(app.outbox.is_empty());

    let _ = app.update(Message::ShowCleanup);
    assert_eq!(app.cleanup_scope(), AccountFilter::Space(perso));
    let count: usize = app.cleanup.senders.iter().map(|s| s.count).sum();
    assert_eq!(count, 5);
    let _ = app.update(Message::Cleanup(CleanupMsg::Filter(AccountFilter::Only(c))));
    // Back to "Perso": `c` is not in it.
    space(&mut app, SpaceMsg::Number(2));
    space(&mut app, SpaceMsg::Number(1));
    assert_eq!(app.cleanup.filter, AccountFilter::All);
}

#[test]
fn ctrl_digits_change_the_space_unless_a_dialog_is_open() {
    let mut app = three_accounts();
    let pro = app.spaces[1].id;
    space(&mut app, SpaceMsg::Number(2));
    assert_eq!(app.space, Some(pro));
    // No third space: nothing changes.
    space(&mut app, SpaceMsg::Number(3));
    assert_eq!(app.space, Some(pro));
    app.overlay = Some(Overlay::Shortcuts);
    space(&mut app, SpaceMsg::Number(0));
    assert_eq!(app.space, Some(pro));
    assert!(SHORTCUTS.iter().any(|(keys, _)| keys.contains("Ctrl+1")));
}

#[test]
fn spaces_are_created_changed_ordered_and_deleted_in_the_settings() {
    let mut app = three_accounts();
    let (a, b, c) = ids3(&app);
    space(&mut app, SpaceMsg::New);
    // Every account ticked at first.
    let Some(Overlay::SpaceEditor(form)) = &app.overlay else {
        panic!("no editor");
    };
    assert_eq!(form.accounts.len(), 3);
    space(&mut app, SpaceMsg::Save);
    let Some(Overlay::SpaceEditor(form)) = &app.overlay else {
        panic!("closed without a name");
    };
    assert!(form.error.is_some());
    space(&mut app, SpaceMsg::Name("  Asso ".into()));
    space(&mut app, SpaceMsg::Icon("bike".into()));
    space(&mut app, SpaceMsg::Color(5));
    space(&mut app, SpaceMsg::Account(a, false));
    space(&mut app, SpaceMsg::Save);
    assert!(app.overlay.is_none());
    assert_eq!(app.spaces.len(), 3);
    let asso = app.spaces[2].clone();
    assert_eq!(asso.name, "Asso");
    assert_eq!(asso.icon, "bike");
    assert_eq!(asso.color, 5);
    assert!(!asso.contains(a) && asso.contains(c));

    space(&mut app, SpaceMsg::Move(asso.id, -1));
    assert_eq!(app.spaces[1].id, asso.id);

    space(&mut app, SpaceMsg::Edit(asso.id));
    space(&mut app, SpaceMsg::Account(b, false));
    space(&mut app, SpaceMsg::Account(c, false));
    space(&mut app, SpaceMsg::Save);
    assert!(app.spaces[1].accounts.is_empty());

    // An empty space shows nothing; deleting the active space shows
    // every account again.
    space(&mut app, SpaceMsg::Select(Some(asso.id)));
    assert!(app.items.is_empty());
    space(&mut app, SpaceMsg::AskDelete(asso.id));
    space(&mut app, SpaceMsg::ConfirmDelete);
    assert_eq!(app.spaces.len(), 2);
    assert_eq!(app.space, None);
    assert_eq!(app.settings.active_space, None);
    assert_eq!(app.items.len(), 6);
}

#[test]
fn the_active_space_is_remembered() {
    let dir = std::env::temp_dir().join(format!("avero-space-{}", std::process::id()));
    let settings_path = crate::settings::path_in(&dir);
    let store = Store::open_in_memory().unwrap();
    let a = add_account_with_mail(&store, "a@x.fr", 1);
    add_account_with_mail(&store, "b@x.fr", 1);
    let perso = store.add_space("Perso", "house", 0, &[a]).unwrap();
    let (mut app, _) = App::new(
        store,
        Backend::Demo,
        Settings::default(),
        Some(settings_path.clone()),
    );
    space(&mut app, SpaceMsg::Select(Some(perso)));
    let saved = Settings::load(&settings_path);
    assert_eq!(saved.active_space, Some(perso.0));

    // Next start: the same space, if it still exists.
    let store = Store::open_in_memory().unwrap();
    let a = add_account_with_mail(&store, "a@x.fr", 1);
    add_account_with_mail(&store, "b@x.fr", 1);
    let again = store.add_space("Perso", "house", 0, &[a]).unwrap();
    assert_eq!(again, perso);
    let (app, _) = App::new(store, Backend::Demo, saved.clone(), None);
    assert_eq!(app.space, Some(perso));
    assert_eq!(app.items.len(), 1);
    let (app, _) = App::new(Store::open_in_memory().unwrap(), Backend::Demo, saved, None);
    assert_eq!(app.space, None);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_deleted_account_leaves_its_spaces() {
    let mut app = three_accounts();
    let (_, b, _) = ids3(&app);
    let pro = app.spaces[1].id;
    space(&mut app, SpaceMsg::Select(Some(pro)));
    let _ = app.update(Message::Account(AccountMsg::AskDelete(b)));
    let _ = app.update(Message::Account(AccountMsg::ConfirmDelete));
    assert!(app.spaces.iter().all(|s| !s.contains(b)));
    assert_eq!(app.spaces[1].accounts.len(), 1);
    assert_eq!(app.items.len(), 1);
}

#[test]
fn every_new_screen_draws() {
    let mut app = app_with(|s| {
        crate::demo::seed(s);
    });
    let pro = app.spaces[1].id;
    space(&mut app, SpaceMsg::Select(Some(pro)));
    let _ = app.view();
    let _ = app.update(Message::MessageSelected(0));
    let _ = app.view();
    let _ = app.update(Message::SearchChanged("budget".into()));
    let _ = app.update(Message::SearchSubmitted);
    let _ = app.update(Message::SearchServer);
    let _ = app.view();
    let _ = app.update(Message::ShowSettings);
    let _ = app.view();
    space(&mut app, SpaceMsg::Edit(pro));
    let _ = app.view();
    space(&mut app, SpaceMsg::AskDelete(pro));
    let _ = app.view();
    let _ = app.update(Message::ShowCleanup);
    let _ = app.view();
}
