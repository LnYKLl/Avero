//! The quick reply under a conversation, and "Chercher aussi sur le
//! serveur" (demo mode, in-memory store).

use super::tests::{add_account_with_mail, app_with};
use super::*;
use crate::view::reader::quick_reply_binding;
use iced::widget::text_editor::{Action, Binding, Edit, KeyPress, Status};

/// The newest message of the inbox, as cached on this computer.
const RAW: &str = "From: Marie <marie@ex.fr>\r\nTo: Camille <moi@x.fr>, paul@ex.fr\r\n\
                   Cc: zoe@ex.fr\r\nSubject: =?utf-8?q?R=C3=A9union?=\r\n\
                   Message-ID: <m1@ex.fr>\r\nReferences: <m0@ex.fr>\r\n\r\nBonjour à tous\r\n";

/// One account (`moi@x.fr`, signed "Camille") whose inbox holds Marie's
/// message, opened in the reader with its body parsed.
fn reading_marie() -> App {
    let mut app = app_with(|s| {
        let me = add_account_with_mail(s, "moi@x.fr", 0);
        s.set_signature(me, "Camille").unwrap();
        let inbox = s.folder_by_role(me, FolderRole::Inbox).unwrap().unwrap();
        let message = avero_core::NewMessage {
            uid: 1,
            message_id: Some("<m1@ex.fr>".into()),
            subject: "Réunion".into(),
            from_name: Some("Marie".into()),
            from_addr: "marie@ex.fr".into(),
            date: 1_700_000_000,
            ..Default::default()
        };
        s.insert_messages(inbox.id, &[message]).unwrap();
        let row = s.message_row(inbox.id, 1).unwrap().unwrap();
        s.put_body(row, RAW.as_bytes(), "Bonjour à tous", false)
            .unwrap();
    });
    let _ = app.update(Message::MessageSelected(0));
    let row = app.reading.as_ref().unwrap().entries[0].item.row_ids[0];
    let opened = crate::background::OpenedBody::parse(RAW.as_bytes());
    let _ = app.update(Message::BodyParsed(row, Ok(Arc::new(opened))));
    app
}

fn type_text(app: &mut App, text: &str) {
    let paste = Action::Edit(Edit::Paste(Arc::new(text.to_string())));
    let _ = app.update(Message::QuickReply(QuickMsg::Edit(paste)));
}

fn quick(app: &mut App, message: QuickMsg) {
    let _ = app.update(Message::QuickReply(message));
}

#[test]
fn quick_reply_answers_everyone_from_the_right_account() {
    let mut app = reading_marie();
    let me = app.accounts[0].id;
    type_text(&mut app, "Je serai là.\n");
    let (from, message) = app.quick_reply_message().unwrap();
    assert_eq!(from, me);
    assert_eq!(message.from_addr, "moi@x.fr");
    let to: Vec<&str> = message.to.iter().map(|r| r.addr.as_str()).collect();
    assert_eq!(to, vec!["marie@ex.fr", "paul@ex.fr"]);
    let cc: Vec<&str> = message.cc.iter().map(|r| r.addr.as_str()).collect();
    assert_eq!(cc, vec!["zoe@ex.fr"]);
    assert_eq!(message.subject, "Re: Réunion");
    assert_eq!(message.in_reply_to.as_deref(), Some("<m1@ex.fr>"));
    assert_eq!(message.references.as_deref(), Some("<m0@ex.fr> <m1@ex.fr>"));
    // The signature, and no quote.
    assert_eq!(message.body, "Je serai là.\n\n-- \nCamille");
    assert!(!message.body.contains("Bonjour"));
}

#[test]
fn quick_reply_is_locked_while_sending_and_emptied_after() {
    let mut app = reading_marie();
    // Nothing typed: nothing sent.
    quick(&mut app, QuickMsg::Send);
    assert!(!app.quick_reply.sending);

    type_text(&mut app, "Merci !");
    quick(&mut app, QuickMsg::Send);
    assert!(app.quick_reply.sending);
    type_text(&mut app, " encore");
    assert_eq!(app.quick_reply.content.text(), "Merci !");
    // Sent: the field is emptied.
    let generation = app.quick_generation;
    quick(&mut app, QuickMsg::Sent(generation, Ok(None)));
    assert!(!app.quick_reply.sending);
    assert!(!app.quick_reply.has_text());
    assert_eq!(app.toast.as_ref().unwrap().text, "Réponse envoyée");

    // Refused: the text stays, with the reason.
    type_text(&mut app, "Second essai");
    quick(&mut app, QuickMsg::Send);
    quick(
        &mut app,
        QuickMsg::Sent(generation, Err("envoi refusé par le serveur".into())),
    );
    assert!(!app.quick_reply.sending);
    assert_eq!(app.quick_reply.content.text(), "Second essai");
    assert_eq!(
        app.quick_reply.error.as_deref(),
        Some("envoi refusé par le serveur")
    );
    // Typing again clears the error.
    type_text(&mut app, ".");
    assert!(app.quick_reply.error.is_none());
}

#[test]
fn quick_reply_of_a_conversation_left_ends_in_a_notice() {
    let mut app = app_with(|s| {
        add_account_with_mail(s, "a@x.fr", 2);
    });
    let _ = app.update(Message::MessageSelected(0));
    let generation = app.quick_generation;
    type_text(&mut app, "brouillon rapide");
    // Another conversation: its field starts empty.
    let _ = app.update(Message::MessageSelected(1));
    assert!(!app.quick_reply.has_text());
    quick(&mut app, QuickMsg::Sent(generation, Err("coupé".into())));
    assert_eq!(
        app.toast.as_ref().unwrap().text,
        "Réponse non envoyée : coupé"
    );
    assert!(app.quick_reply.error.is_none());
}

#[test]
fn quick_reply_opens_the_full_composer_with_the_text() {
    let mut app = reading_marie();
    type_text(&mut app, "Bonne idée");
    quick(&mut app, QuickMsg::Expand);
    let composer = app.composer.as_ref().unwrap();
    let body = composer.body.text();
    assert!(body.starts_with("Bonne idée\n\n-- \nCamille\n\n"), "{body}");
    assert!(body.contains("a écrit :"), "{body}");
    assert_eq!(composer.cc, "zoe@ex.fr");
    assert!(!app.quick_reply.has_text());
}

#[test]
fn quick_reply_keys_send_with_ctrl_enter_and_keep_the_letters() {
    use iced::keyboard::{Key, Modifiers, key};
    let press = |key: Key, modifiers, status, text: Option<&str>| KeyPress {
        key: key.clone(),
        modified_key: key,
        physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
        modifiers,
        text: text.map(Into::into),
        status,
    };
    let focused = Status::Focused { is_hovered: false };
    let enter = || Key::Named(key::Named::Enter);
    let sends = quick_reply_binding(press(enter(), Modifiers::CTRL, focused, None));
    assert!(matches!(
        sends,
        Some(Binding::Custom(Message::QuickReply(QuickMsg::Send)))
    ));
    // Not focused: the key is not the field's.
    assert!(quick_reply_binding(press(enter(), Modifiers::CTRL, Status::Active, None)).is_none());
    // A letter typed in the field stays there (the one-key shortcuts
    // never see it).
    let letter = quick_reply_binding(press(
        Key::Character("e".into()),
        Modifiers::empty(),
        focused,
        Some("e"),
    ));
    assert!(matches!(letter, Some(Binding::Insert('e'))));
    let newline = quick_reply_binding(press(enter(), Modifiers::empty(), focused, None));
    assert!(matches!(newline, Some(Binding::Enter)));
}

// ------------------------------------------------------ search on the server

fn search(app: &mut App, text: &str) {
    let _ = app.update(Message::SearchChanged(text.into()));
    let _ = app.update(Message::SearchSubmitted);
}

#[test]
fn server_search_runs_one_job_per_account_and_adds_what_it_found() {
    let mut app = app_with(|s| {
        crate::demo::seed(s);
    });
    search(&mut app, "budget");
    let local = app.items.len();
    assert!(matches!(app.server_search, ServerSearch::Idle));
    let _ = app.update(Message::SearchServer);
    let ServerSearch::Running { waiting, .. } = &app.server_search else {
        panic!("not running");
    };
    assert_eq!(*waiting, 3);
    assert!(app.is_waiting());
    // The demo answers after a moment; deliver its answers by hand.
    let generation = app.server_generation;
    let ids: Vec<AccountId> = app.accounts.iter().map(|a| a.id).collect();
    for (index, id) in ids.iter().enumerate() {
        let hits = crate::demo::server_hits(&app.store, *id, "budget");
        let event = if index == 2 {
            Event::Gone
        } else {
            Event::Found(Ok(hits))
        };
        let _ = app.update(Message::ServerFound(generation, *id, event));
    }
    let ServerSearch::Done { added, failed, .. } = &app.server_search else {
        panic!("not done");
    };
    assert_eq!((*added, *failed), (0, 1));
    // Added by the demo job itself (at the click): found again here.
    assert_eq!(app.items.len(), local + 3);
    assert_eq!(server_search_result(2, 0), "2 mails trouvés sur le serveur");
    assert_eq!(server_search_result(1, 0), "1 mail trouvé sur le serveur");
    assert_eq!(
        server_search_result(0, 1),
        "Aucun autre mail trouvé sur le serveur (un compte n'a pas pu chercher)"
    );
}

#[test]
fn server_results_show_even_when_the_text_is_not_here() {
    let mut app = app_with(|s| {
        add_account_with_mail(s, "a@x.fr", 2);
    });
    let a = app.accounts[0].id;
    search(&mut app, "contrat");
    assert!(app.items.is_empty());
    let _ = app.update(Message::SearchServer);
    // Found by the server in its text, which is not on this computer.
    let archive = app
        .store
        .upsert_folder(a, "Archives", FolderRole::Archive)
        .unwrap();
    let message = avero_core::NewMessage {
        uid: 9,
        message_id: Some("<old@x>".into()),
        subject: "Signature".into(),
        from_addr: "notaire@x.fr".into(),
        date: 1_600_000_000,
        ..Default::default()
    };
    app.store.insert_messages(archive.id, &[message]).unwrap();
    let rows = app.store.rows_of_uids(archive.id, &[9]).unwrap();
    let hits = avero_sync::search::ServerHits { added: 1, rows };
    let generation = app.server_generation;
    let _ = app.update(Message::ServerFound(generation, a, Event::Found(Ok(hits))));
    // With the demo's own made-up result (added at the click).
    let subjects: Vec<&str> = app.items.iter().map(|i| i.subject.as_str()).collect();
    assert_eq!(subjects, vec!["contrat (message plus ancien)", "Signature"]);
    assert!(matches!(
        app.server_search,
        ServerSearch::Done { added: 1, .. }
    ));
}

#[test]
fn a_new_search_cancels_the_one_on_the_server() {
    let mut app = app_with(|s| {
        add_account_with_mail(s, "a@x.fr", 2);
    });
    let a = app.accounts[0].id;
    search(&mut app, "Sujet");
    let _ = app.update(Message::SearchServer);
    let generation = app.server_generation;
    assert!(app.server_search.is_running());
    search(&mut app, "Sujet 1");
    assert!(matches!(app.server_search, ServerSearch::Idle));
    // A late answer to the old search changes nothing.
    let hits = avero_sync::search::ServerHits {
        added: 4,
        rows: vec![1, 2],
    };
    let _ = app.update(Message::ServerFound(generation, a, Event::Found(Ok(hits))));
    assert!(matches!(app.server_search, ServerSearch::Idle));
    assert!(app.server_rows.is_empty());
    // Same text on other accounts: asked again too.
    let _ = app.update(Message::SearchServer);
    assert!(app.server_search.is_running());
    let _ = app.update(Message::FilterSelected(AccountFilter::Only(a)));
    assert!(matches!(app.server_search, ServerSearch::Idle));
    // Leaving the search stops it.
    let _ = app.update(Message::SearchServer);
    let _ = app.update(Message::SearchCleared);
    assert!(matches!(app.server_search, ServerSearch::Idle));
}

#[test]
fn server_search_needs_words() {
    let mut app = app_with(|s| {
        add_account_with_mail(s, "a@x.fr", 1);
    });
    search(&mut app, " -- ");
    let _ = app.update(Message::SearchServer);
    assert!(matches!(app.server_search, ServerSearch::Idle));
}

// ------------------------------------------- answers closed untouched

fn compose(app: &mut App, message: ComposeMsg) {
    let _ = app.update(Message::Compose(message));
}

#[test]
fn reply_or_forward_closed_untouched_leaves_no_draft() {
    for mode in [Mode::Reply, Mode::Forward] {
        let mut app = reading_marie();
        let _ = app.update(Message::Answer(mode));
        assert!(app.composer.is_some());
        compose(&mut app, ComposeMsg::Cancel);
        assert!(app.composer.is_none());
        assert!(app.store.closed_drafts().unwrap().is_empty(), "{mode:?}");
        assert!(app.store.open_drafts().unwrap().is_empty(), "{mode:?}");
    }
    // Written in: kept.
    let mut app = reading_marie();
    let _ = app.update(Message::Answer(Mode::Reply));
    compose(&mut app, ComposeMsg::Subject("Re: Réunion de jeudi".into()));
    compose(&mut app, ComposeMsg::Cancel);
    assert_eq!(app.store.closed_drafts().unwrap().len(), 1);
}

#[test]
fn reply_scheduled_untouched_is_kept() {
    let mut app = reading_marie();
    let _ = app.update(Message::Answer(Mode::Reply));
    let _ = app.update(Message::Later(LaterMsg::SendLater));
    let _ = app.update(Message::Later(LaterMsg::Pick(crate::later::When::Tomorrow)));
    assert!(app.composer.is_none());
    assert_eq!(app.outbox.len(), 1);
}

#[test]
fn untouched_answers_kept_by_an_older_version_go_at_start() {
    let app = app_with(|s| {
        let me = add_account_with_mail(s, "moi@x.fr", 0);
        s.set_signature(me, "Camille").unwrap();
        let fields = |subject: &str, to: &str, body: &str| avero_core::DraftFields {
            account: me,
            to: to.into(),
            cc: String::new(),
            bcc: String::new(),
            subject: subject.into(),
            body: body.into(),
            in_reply_to: Some("<m1@ex.fr>".into()),
            references: Some("<m1@ex.fr>".into()),
            message_id: format!("<{subject}@x.fr>"),
        };
        let quote =
            "\n\n-- \nCamille\n\nLe 14/11/2023 à 23:13, Marie a écrit :\n> Bonjour à tous\n";
        let drafts = [
            // What closing an untouched reply kept.
            fields("Re: Réunion", "Marie <marie@ex.fr>", quote),
            // A reply with a line written: it stays.
            fields("Re: Réunion", "marie@ex.fr", &format!("J'y serai.{quote}")),
        ];
        for draft in drafts {
            let id = s.save_draft(None, &draft, None, 10).unwrap();
            s.set_draft_open(id, false).unwrap();
        }
    });
    let left = app.store.closed_drafts().unwrap();
    assert_eq!(left.len(), 1);
    assert!(left[0].fields.body.starts_with("J'y serai."));
}
