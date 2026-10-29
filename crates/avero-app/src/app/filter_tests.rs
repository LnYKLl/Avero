//! Tests of "Spam" / "Pas spam", the spam filter on new mail, the rules
//! and "Grand nettoyage", on an in-memory store in demo mode.

use super::*;
use avero_core::rules::{Condition, RuleDef, RuleField, RuleMove, block_rule};
use avero_core::spam::Learned;
use avero_core::{FolderId, NewAccount, NewMessage};
use avero_sync::sync::Arrival;

/// An account with an inbox, a spam folder, a trash and a sent folder.
fn account(store: &Store, email: &str) -> (AccountId, FolderId, FolderId) {
    let server = ServerConfig {
        host: "imap.example.com".to_string(),
        port: 993,
        username: email.to_string(),
        security: Security::Tls,
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
    let junk = store
        .upsert_folder(account.id, "Spam", FolderRole::Junk)
        .unwrap();
    store
        .upsert_folder(account.id, "Trash", FolderRole::Trash)
        .unwrap();
    store
        .upsert_folder(account.id, "Sent", FolderRole::Sent)
        .unwrap();
    (account.id, inbox.id, junk.id)
}

fn mail(uid: u32, from: &str, subject: &str) -> NewMessage {
    NewMessage {
        uid,
        message_id: Some(format!("<{uid}.{from}>")),
        subject: subject.to_string(),
        from_name: None,
        from_addr: from.to_string(),
        date: 1_700_000_000 + i64::from(uid),
        seen: false,
        size: Some(1000 * uid),
        ..Default::default()
    }
}

fn row_of(app: &App, folder: FolderId, uid: u32) -> Option<i64> {
    app.store
        .message_refs(&(1..500).collect::<Vec<i64>>())
        .unwrap()
        .into_iter()
        .find(|r| r.folder_id == folder && r.uid == uid)
        .map(|r| r.row_id)
}

/// Whether the row is hidden (moving on the server) or gone.
fn moving(app: &App, row: i64) -> bool {
    !visible_rows(app).contains(&row)
}

/// Every row some list shows (inbox, spam folder, archive).
fn visible_rows(app: &App) -> Vec<i64> {
    [FolderRole::Inbox, FolderRole::Junk, FolderRole::Archive]
        .into_iter()
        .flat_map(|role| app.store.unified(role, AccountFilter::All, 500, 0).unwrap())
        .flat_map(|item| item.row_ids)
        .collect()
}

fn seen(app: &App, row: i64) -> bool {
    app.store
        .unified(FolderRole::Inbox, AccountFilter::All, 500, 0)
        .unwrap()
        .into_iter()
        .find(|i| i.row_ids.contains(&row))
        .is_some_and(|i| i.seen)
}

fn app_with(setup: impl FnOnce(&Store)) -> App {
    let store = Store::open_in_memory().unwrap();
    setup(&store);
    let (app, _task) = App::new(store, Backend::Demo, Settings::default(), None);
    app
}

/// Teaches the filter 25 casino spams and 25 meeting mails.
fn train(store: &Store, inbox: FolderId, junk: FolderId) {
    for i in 0..25 {
        store
            .insert_messages(
                junk,
                &[mail(
                    1000 + i,
                    "win@casino-gold.biz",
                    "Gagnez gros au casino bonus",
                )],
            )
            .unwrap();
        store
            .insert_messages(
                inbox,
                &[mail(2000 + i, "lea@atelier.fr", "Réunion projet planning")],
            )
            .unwrap();
    }
}

fn arrival(uid: u32, from: &str, subject: &str) -> Arrival {
    Arrival {
        uid,
        from_name: None,
        from_addr: from.to_string(),
        subject: subject.to_string(),
        seen: false,
    }
}

#[test]
fn spam_then_not_spam_moves_and_teaches() {
    let mut inbox = FolderId(0);
    let mut junk = FolderId(0);
    let mut app = app_with(|s| {
        let (_, i, j) = account(s, "moi@x.fr");
        (inbox, junk) = (i, j);
        s.insert_messages(i, &[mail(1, "promo@shop.fr", "Promo")])
            .unwrap();
        s.insert_messages(j, &[mail(2, "ami@x.fr", "Photos")])
            .unwrap();
    });
    // At start the filter learned from the folders (once).
    let learned = |app: &App| app.store.spam_learned().unwrap();
    assert_eq!(learned(&app), Learned { spam: 1, ham: 1 });
    // "Spam" on the inbox message: learned the other way now, hidden
    // while it moves.
    let row = row_of(&app, inbox, 1).unwrap();
    let _ = app.update(Message::MessageSelected(0));
    assert!(!app.selection_in_junk());
    let _ = app.update(Message::MarkSpam(true));
    assert!(moving(&app, row));
    assert_eq!(learned(&app), Learned { spam: 2, ham: 0 });
    let _ = app.update(Message::Moved(vec![row], Event::Done(Ok(()))));
    assert!(row_of(&app, inbox, 1).is_none(), "gone once moved");

    // "Pas spam" with `!` in the spam folder: back to the inbox, and the
    // sender is never filtered again.
    let _ = app.update(Message::FolderSelected(FolderRole::Junk));
    let row = row_of(&app, junk, 2).unwrap();
    let _ = app.update(Message::MessageSelected(0));
    assert!(app.selection_in_junk());
    let _ = app.on_key(
        keyboard::Key::Character("!".into()),
        keyboard::Modifiers::default(),
    );
    assert!(moving(&app, row));
    assert_eq!(learned(&app), Learned { spam: 1, ham: 1 });
    assert!(app.store.spam_exempt("ami@x.fr").unwrap());
}

#[test]
fn shortcut_help_lists_the_spam_key() {
    assert!(SHORTCUTS.iter().any(|(keys, _)| *keys == "!"));
}

#[test]
fn new_mail_goes_through_the_rules_then_the_spam_filter() {
    let mut ids = (AccountId(0), FolderId(0), FolderId(0));
    let mut app = app_with(|s| {
        ids = account(s, "moi@x.fr");
        let (_, inbox, junk) = ids;
        train(s, inbox, junk);
        // Someone the user wrote to, who looks like spam.
        let sent = s.folder_by_role(ids.0, FolderRole::Sent).unwrap().unwrap();
        let mut answer = mail(1, "moi@x.fr", "Re: casino");
        answer.headers.to = vec!["copain@casino-gold.biz".to_string()];
        s.insert_messages(sent.id, &[answer]).unwrap();
        s.add_rule(&block_rule("bloque@pub.fr", "")).unwrap();
        s.add_rule(&RuleDef {
            name: "Factures lues".into(),
            conditions: vec![Condition {
                field: RuleField::Subject,
                value: "facture".into(),
            }],
            match_all: true,
            mark_read: true,
            move_to: None,
            account: None,
        })
        .unwrap();
        // The new mail of the sync.
        s.insert_messages(
            inbox,
            &[
                mail(1, "bloque@pub.fr", "Votre facture"),
                mail(2, "edf@edf.fr", "Votre facture d'octobre"),
                mail(3, "win@casino-gold.biz", "Bonus casino : gagnez gros"),
                mail(4, "copain@casino-gold.biz", "Gagnez gros au casino bonus"),
                mail(5, "lea@atelier.fr", "Planning projet"),
            ],
        )
        .unwrap();
    });
    let (account_id, inbox, junk) = ids;
    // The filter already learned from the folders (once, after a sync).
    let _ = app.update(Message::Worker(account_id, Event::Synced(Ok(true))));
    assert!(app.store.spam_bootstrapped(account_id).unwrap());
    let _ = junk;

    let rows: Vec<i64> = (1..=5)
        .map(|uid| row_of(&app, inbox, uid).unwrap())
        .collect();
    let arrivals = vec![
        arrival(1, "bloque@pub.fr", "Votre facture"),
        arrival(2, "edf@edf.fr", "Votre facture d'octobre"),
        arrival(3, "win@casino-gold.biz", "Bonus casino : gagnez gros"),
        arrival(4, "copain@casino-gold.biz", "Gagnez gros au casino bonus"),
        arrival(5, "lea@atelier.fr", "Planning projet"),
    ];
    let facts = app
        .store
        .arrival_facts(account_id, &[1, 2, 3, 4, 5])
        .unwrap();
    let plan = app.plan_for_new_mail(&facts);
    assert_eq!(plan.moves[&Destination::Trash], vec![rows[0]], "blocked");
    assert_eq!(plan.moves[&Destination::Junk], vec![rows[2]], "spam");
    // The first message matches both rules: moved and marked read.
    assert_eq!(plan.read, vec![rows[0], rows[1]]);
    assert_eq!(plan.spam, 1);

    let _ = app.update(Message::Background(account_id, Event::NewMail(arrivals)));
    assert!(moving(&app, rows[0]));
    assert!(seen(&app, rows[1]) && !moving(&app, rows[1]));
    assert!(moving(&app, rows[2]));
    assert!(!moving(&app, rows[3]), "written to: never filtered");
    assert!(!moving(&app, rows[4]) && !seen(&app, rows[4]));
}

#[test]
fn spam_filter_switched_off_moves_nothing() {
    let mut ids = (AccountId(0), FolderId(0), FolderId(0));
    let mut app = app_with(|s| {
        ids = account(s, "moi@x.fr");
        train(s, ids.1, ids.2);
        s.insert_messages(
            ids.1,
            &[mail(1, "win@casino-gold.biz", "casino bonus gagnez")],
        )
        .unwrap();
    });
    let _ = app.update(Message::Worker(ids.0, Event::Synced(Ok(true))));
    let facts = app.store.arrival_facts(ids.0, &[1]).unwrap();
    assert_eq!(app.plan_for_new_mail(&facts).spam, 1);
    let _ = app.update(Message::SpamFilterToggled(false));
    assert!(!app.settings.spam_filter);
    assert_eq!(app.plan_for_new_mail(&facts), filters::Plan::default());
}

#[test]
fn not_enough_learning_moves_nothing() {
    let mut ids = (AccountId(0), FolderId(0), FolderId(0));
    let mut app = app_with(|s| {
        ids = account(s, "moi@x.fr");
        s.insert_messages(ids.2, &[mail(9, "win@casino-gold.biz", "casino bonus")])
            .unwrap();
        s.insert_messages(ids.1, &[mail(1, "win@casino-gold.biz", "casino bonus")])
            .unwrap();
    });
    let _ = app.update(Message::Worker(ids.0, Event::Synced(Ok(true))));
    let facts = app.store.arrival_facts(ids.0, &[1]).unwrap();
    assert_eq!(app.plan_for_new_mail(&facts).spam, 0);
}

#[test]
fn rule_editor_checks_then_saves_in_order() {
    let mut app = app_with(|s| {
        account(s, "moi@x.fr");
    });
    let _ = app.update(Message::Rule(RuleMsg::New));
    let _ = app.update(Message::Rule(RuleMsg::Save));
    let error = |app: &App| match &app.overlay {
        Some(Overlay::RuleEditor(form)) => form.error.clone(),
        _ => None,
    };
    assert_eq!(
        error(&app).as_deref(),
        Some("Indiquez au moins une condition")
    );
    let _ = app.update(Message::Rule(RuleMsg::Value(0, " promo@ ".into())));
    let _ = app.update(Message::Rule(RuleMsg::MoveTo(None)));
    let _ = app.update(Message::Rule(RuleMsg::Save));
    assert_eq!(
        error(&app).as_deref(),
        Some("Choisissez au moins une action")
    );
    let _ = app.update(Message::Rule(RuleMsg::MarkRead(true)));
    let _ = app.update(Message::Rule(RuleMsg::AddCondition));
    let _ = app.update(Message::Rule(RuleMsg::Field(1, RuleField::To)));
    let _ = app.update(Message::Rule(RuleMsg::Value(1, "pro@".into())));
    let _ = app.update(Message::Rule(RuleMsg::MatchAll(false)));
    let _ = app.update(Message::Rule(RuleMsg::Save));
    assert!(app.overlay.is_none());
    assert_eq!(app.rules.len(), 1);
    let first = &app.rules[0];
    assert_eq!(first.def.name, "Expéditeur contient « promo@ »");
    assert_eq!(first.def.conditions.len(), 2);
    assert!(!first.def.match_all && first.def.mark_read);

    // A second rule, then moved before the first one.
    let _ = app.update(Message::Rule(RuleMsg::New));
    let _ = app.update(Message::Rule(RuleMsg::Name("Spam maison".into())));
    let _ = app.update(Message::Rule(RuleMsg::Value(0, "casino".into())));
    let _ = app.update(Message::Rule(RuleMsg::MoveTo(Some(RuleMove::Junk))));
    let _ = app.update(Message::Rule(RuleMsg::Save));
    let second = app.rules[1].id;
    let _ = app.update(Message::Rule(RuleMsg::Shift(second, true)));
    assert_eq!(app.rules[0].def.name, "Spam maison");
    // Edited, switched off, deleted.
    let _ = app.update(Message::Rule(RuleMsg::Edit(second)));
    let _ = app.update(Message::Rule(RuleMsg::Name("Casino".into())));
    let _ = app.update(Message::Rule(RuleMsg::Save));
    assert_eq!(app.rules[0].def.name, "Casino");
    let _ = app.update(Message::Rule(RuleMsg::Toggle(second, false)));
    assert!(!app.rules[0].enabled);
    let _ = app.update(Message::Rule(RuleMsg::Delete(second)));
    assert_eq!(app.rules.len(), 1);
}

#[test]
fn apply_now_counts_then_acts_on_the_inbox() {
    let mut ids = (AccountId(0), FolderId(0), FolderId(0));
    let mut app = app_with(|s| {
        ids = account(s, "moi@x.fr");
        let mut read = mail(3, "news@shop.fr", "Lu");
        read.seen = true;
        s.insert_messages(
            ids.1,
            &[
                mail(1, "news@shop.fr", "Promo"),
                mail(2, "ami@x.fr", "Salut"),
                read,
            ],
        )
        .unwrap();
        s.add_rule(&RuleDef {
            name: "Shop".into(),
            conditions: vec![Condition {
                field: RuleField::From,
                value: "shop.fr".into(),
            }],
            match_all: true,
            mark_read: true,
            move_to: None,
            account: Some(ids.0),
        })
        .unwrap();
    });
    let rule = app.rules[0].id;
    let _ = app.update(Message::Rule(RuleMsg::Apply(rule)));
    // Only the unread one changes (the read one is already read).
    assert!(matches!(
        app.overlay,
        Some(Overlay::ConfirmApplyRule { count: 1, .. })
    ));
    let _ = app.update(Message::Rule(RuleMsg::ConfirmApply));
    assert!(app.overlay.is_none());
    assert!(seen(&app, row_of(&app, ids.1, 1).unwrap()));
    assert!(!seen(&app, row_of(&app, ids.1, 2).unwrap()));
    // Nothing left to do: says so without asking.
    let _ = app.update(Message::Rule(RuleMsg::Apply(rule)));
    assert!(app.overlay.is_none());
}

#[test]
fn big_clean_up_lists_trashes_and_blocks() {
    let mut ids = (AccountId(0), FolderId(0), FolderId(0));
    let mut app = app_with(|s| {
        ids = account(s, "moi@x.fr");
        let mut news: Vec<NewMessage> = (1..=3)
            .map(|uid| mail(uid, "news@shop.fr", "Promo"))
            .collect();
        news[0].headers.list_unsubscribe = Some("<https://shop.fr/u>".into());
        news[2].seen = true;
        s.insert_messages(ids.1, &news).unwrap();
        s.insert_messages(ids.1, &[mail(4, "ami@x.fr", "Salut")])
            .unwrap();
    });
    let _ = app.update(Message::Worker(ids.0, Event::Synced(Ok(true))));
    let _ = app.update(Message::ShowCleanup);
    assert_eq!(app.screen, Screen::Cleanup);
    let senders: Vec<(&str, usize, usize)> = app
        .cleanup
        .senders
        .iter()
        .map(|s| (s.addr.as_str(), s.count, s.unread))
        .collect();
    assert_eq!(senders, [("news@shop.fr", 3, 2), ("ami@x.fr", 1, 1)]);
    assert_eq!(app.cleanup.senders[0].size, 6000);
    assert!(!app.cleanup.more);

    // Unsubscribing asks first, with the newest offer.
    let _ = app.update(Message::Cleanup(CleanupMsg::Unsubscribe(
        "news@shop.fr".into(),
    )));
    assert!(matches!(app.overlay, Some(Overlay::ConfirmUnsubscribe(_))));
    let _ = app.update(Message::Account(AccountMsg::CloseDialog));

    // Block = a rule sending their mail to the trash.
    let _ = app.update(Message::Cleanup(CleanupMsg::Block("news@shop.fr".into())));
    assert!(app.is_blocked("news@shop.fr"));
    assert_eq!(app.rules.len(), 1);
    assert_eq!(app.rules[0].def.move_to, Some(RuleMove::Trash));
    let _ = app.update(Message::Cleanup(CleanupMsg::Block("news@shop.fr".into())));
    assert_eq!(app.rules.len(), 1, "blocked once");

    // Everything to the trash, after a confirmation with the count.
    let _ = app.update(Message::Cleanup(CleanupMsg::TrashAll(
        "news@shop.fr".into(),
    )));
    assert!(matches!(
        &app.overlay,
        Some(Overlay::ConfirmTrashSender { rows, .. }) if rows.len() == 3
    ));
    let _ = app.update(Message::Cleanup(CleanupMsg::ConfirmTrash));
    assert!(app.overlay.is_none());
    let left: Vec<&str> = app
        .cleanup
        .senders
        .iter()
        .map(|s| s.addr.as_str())
        .collect();
    assert_eq!(left, ["ami@x.fr"], "hidden while moving");
}

#[test]
fn big_clean_up_pages_and_filters_by_account() {
    let mut a = AccountId(0);
    let mut app = app_with(|s| {
        let (id, inbox, _) = account(s, "moi@x.fr");
        a = id;
        let many: Vec<NewMessage> = (1..=(cleanup::CLEANUP_PAGE as u32 + 5))
            .map(|uid| mail(uid, &format!("s{uid}@x.fr"), "Bonjour"))
            .collect();
        s.insert_messages(inbox, &many).unwrap();
        let (_, other, _) = account(s, "pro@x.fr");
        s.insert_messages(other, &[mail(1, "client@pme.fr", "Devis")])
            .unwrap();
    });
    let _ = app.update(Message::ShowCleanup);
    assert_eq!(app.cleanup.senders.len(), cleanup::CLEANUP_PAGE);
    assert!(app.cleanup.more);
    let _ = app.update(Message::Cleanup(CleanupMsg::More));
    assert_eq!(app.cleanup.senders.len(), cleanup::CLEANUP_PAGE + 6);
    assert!(!app.cleanup.more);
    let _ = app.update(Message::Cleanup(CleanupMsg::Filter(AccountFilter::Only(a))));
    assert_eq!(app.cleanup.senders.len(), cleanup::CLEANUP_PAGE);
    assert!(
        app.cleanup
            .senders
            .iter()
            .all(|s| s.addr != "client@pme.fr")
    );
}

#[test]
fn demo_has_rules_spam_and_senders_to_clean() {
    let store = crate::demo::seeded_store();
    let (mut app, _task) = App::new(store, Backend::Demo, Settings::default(), None);
    assert!(app.rules.len() >= 3);
    let _ = app.update(Message::FolderSelected(FolderRole::Junk));
    assert!(app.items.len() >= 3);
    assert!(app.items.iter().any(|i| !i.seen));
    let _ = app.update(Message::ShowCleanup);
    let monde = app
        .cleanup
        .senders
        .iter()
        .find(|s| s.addr == "newsletter@lemonde.fr")
        .unwrap();
    assert!(
        monde.count >= 4 && monde.size > 0 && monde.can_unsubscribe,
        "{monde:?}"
    );
    let counts: Vec<usize> = app.cleanup.senders.iter().map(|s| s.count).collect();
    assert!(counts.windows(2).all(|w| w[0] >= w[1]), "{counts:?}");
    // The settings page with its rules can be drawn.
    let _ = app.update(Message::ShowSettings);
    let _ = app.view();
    let _ = app.update(Message::ShowCleanup);
    let _ = app.view();
    let _ = app.update(Message::Rule(RuleMsg::Edit(app.rules[0].id)));
    let _ = app.view();
}

#[test]
fn rules_also_move_new_mail_read_elsewhere_without_notifying_it() {
    let mut ids = (AccountId(0), FolderId(0), FolderId(0));
    let mut app = app_with(|s| {
        ids = account(s, "moi@x.fr");
        s.add_rule(&RuleDef {
            name: "Lettres".into(),
            conditions: vec![Condition {
                field: RuleField::From,
                value: "lettre@".into(),
            }],
            match_all: true,
            mark_read: false,
            move_to: Some(RuleMove::Trash),
            account: None,
        })
        .unwrap();
        let mut read = mail(1, "lettre@journal.fr", "La lettre du jour");
        read.seen = true;
        let unread = mail(2, "lea@atelier.fr", "Planning");
        let mut read_too = mail(3, "lea@atelier.fr", "Planning (lu)");
        read_too.seen = true;
        s.insert_messages(ids.1, &[read, unread, read_too]).unwrap();
    });
    let (account_id, inbox, _) = ids;
    let rows: Vec<i64> = (1..=3)
        .map(|uid| row_of(&app, inbox, uid).unwrap())
        .collect();
    let mut arrivals = vec![
        arrival(1, "lettre@journal.fr", "La lettre du jour"),
        arrival(2, "lea@atelier.fr", "Planning"),
        arrival(3, "lea@atelier.fr", "Planning (lu)"),
    ];
    arrivals[0].seen = true;
    arrivals[2].seen = true;
    // Only the unread one that stays is worth a notification.
    let facts = app.store.arrival_facts(account_id, &[1, 2, 3]).unwrap();
    let plan = app.plan_for_new_mail(&facts);
    let told = super::filters::to_notify(arrivals.clone(), &facts, &plan.rows());
    let told: Vec<u32> = told.iter().map(|a| a.uid).collect();
    assert_eq!(told, vec![2]);
    // The rule moves the letter read on the phone.
    let _ = app.update(Message::Background(account_id, Event::NewMail(arrivals)));
    assert!(moving(&app, rows[0]));
    assert!(!moving(&app, rows[1]) && !moving(&app, rows[2]));
}
