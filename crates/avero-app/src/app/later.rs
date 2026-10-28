//! "Plus tard" (a conversation put away until a chosen time) and
//! "Envoyer plus tard" (mail kept here until its time, listed in
//! "Programmés"), with the timer that wakes the one and sends the other.
//!
//! Both are local: nothing moves on the server before the time comes,
//! and Avero must be running for it to come (a reminder or a mail due
//! while it was closed comes at the next start).

use super::{App, Message, Overlay, Screen};
use crate::compose::{self, Attached};
use crate::later::{self, When};
use crate::notify::{self, Notice};
use crate::worker::{Event, Job};
use avero_core::{DraftRecord, Woken};
use iced::Task;
use std::sync::Arc;
use std::time::Duration;

/// How often the timer looks for what is due.
pub const TICK: Duration = Duration::from_secs(30);

/// A scheduled mail that leaves more than this late (Avero was closed at
/// its time) stays in "Programmés" to say so.
pub const LATE_AFTER: i64 = 120;

/// Error given to a scheduled mail whose sending was cut short.
pub const INTERRUPTED: &str = "Envoi interrompu par la fermeture d'Avero : vérifiez « Envoyés » \
     avant de l'envoyer à nouveau";

/// Messages of "Plus tard" and "Programmés".
#[derive(Debug, Clone)]
pub enum LaterMsg {
    /// "Plus tard" for the lines acted on: opens the choices.
    Snooze,
    /// "Annuler le rappel" for the lines acted on.
    Unsnooze,
    /// "Envoyer plus tard" in the composer: opens the choices.
    SendLater,
    /// A ready-made choice of the dialog.
    Pick(When),
    /// The date typed in the dialog.
    CustomText(String),
    /// "Valider" for the typed date.
    PickCustom,
    /// The timer: wakes the snoozed mail and sends the scheduled mail
    /// that are due.
    Tick,
    /// The "Plus tard" list.
    ShowSnoozed,
    /// The "Programmés" list.
    ShowScheduled,
    /// "Modifier" a scheduled mail: back in the composer.
    Edit(i64),
    /// "Envoyer maintenant" a scheduled mail.
    SendNow(i64),
    /// "Annuler l'envoi" of a scheduled mail (it is deleted).
    Cancel(i64),
    /// "Masquer" a scheduled mail that left late.
    Dismiss(i64),
    /// What a worker answered about scheduled mail `.0`.
    Sent(i64, Event),
    /// What a worker answered about saving draft `.0` on the server.
    DraftSaved(i64, Event),
}

/// What the time chosen in the dialog is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaterPurpose {
    /// Snoozing the conversations of these rows.
    Snooze(Vec<i64>),
    /// Sending the composer's message.
    Send,
}

/// The dialog of "Plus tard" and "Envoyer plus tard".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaterPicker {
    pub purpose: LaterPurpose,
    /// The date typed (`JJ/MM/AAAA HH:MM`).
    pub custom: String,
    pub error: Option<String>,
}

impl LaterPicker {
    /// The ready-made choices offered.
    pub fn choices(&self) -> &'static [When] {
        match self.purpose {
            LaterPurpose::Snooze(_) => &When::SNOOZE,
            LaterPurpose::Send => &When::SEND,
        }
    }
}

impl App {
    pub(super) fn update_later(&mut self, message: LaterMsg) -> Task<Message> {
        match message {
            LaterMsg::Snooze => {
                let rows = self.action_rows();
                if !rows.is_empty() && self.screen == Screen::Mail {
                    self.open_picker(LaterPurpose::Snooze(rows));
                }
                Task::none()
            }
            LaterMsg::Unsnooze => self.unsnooze(),
            LaterMsg::SendLater => self.ask_send_later(),
            LaterMsg::Pick(when) => {
                let at = later::resolve(when, &self.clock.local()).timestamp();
                self.picked(at)
            }
            LaterMsg::CustomText(text) => {
                if let Some(Overlay::Later(picker)) = &mut self.overlay {
                    picker.custom = text;
                    picker.error = None;
                }
                Task::none()
            }
            LaterMsg::PickCustom => {
                let Some(Overlay::Later(picker)) = &mut self.overlay else {
                    return Task::none();
                };
                match later::parse_custom(&picker.custom, &self.clock.local()) {
                    Ok(at) => self.picked(at.timestamp()),
                    Err(error) => {
                        picker.error = Some(error);
                        Task::none()
                    }
                }
            }
            LaterMsg::Tick => self.tick(),
            LaterMsg::ShowSnoozed => {
                self.show_place(true, false);
                Task::none()
            }
            LaterMsg::ShowScheduled => {
                self.show_place(false, true);
                Task::none()
            }
            LaterMsg::Edit(id) => self.edit_scheduled(id),
            LaterMsg::SendNow(id) => match self.store.draft(id) {
                Ok(Some(record)) if !self.sending.contains(&id) => self.send_scheduled(record),
                _ => Task::none(),
            },
            LaterMsg::Cancel(id) => {
                if self.sending.contains(&id) {
                    return Task::none();
                }
                let _ = self.store.delete_draft(id);
                self.refresh_outbox();
                self.toast("Envoi annulé", false)
            }
            LaterMsg::Dismiss(id) => {
                let _ = self.store.delete_draft(id);
                self.refresh_outbox();
                Task::none()
            }
            LaterMsg::Sent(id, event) => self.scheduled_answer(id, event),
            LaterMsg::DraftSaved(id, event) => self.draft_saved(id, event),
        }
    }

    /// Shows "Plus tard" or "Programmés" in the list.
    fn show_place(&mut self, snoozed: bool, scheduled: bool) {
        self.snoozed = snoozed;
        self.scheduled = scheduled;
        self.starred = false;
        self.sender = None;
        self.search_text.clear();
        self.search_query = None;
        self.screen = Screen::Mail;
        self.load_first_page();
    }

    fn open_picker(&mut self, purpose: LaterPurpose) {
        self.overlay = Some(Overlay::Later(LaterPicker {
            purpose,
            custom: String::new(),
            error: None,
        }));
    }

    /// A time was chosen in the dialog.
    fn picked(&mut self, at: i64) -> Task<Message> {
        let Some(Overlay::Later(picker)) = self.overlay.take() else {
            return Task::none();
        };
        match picker.purpose {
            LaterPurpose::Snooze(rows) => self.snooze(&rows, at),
            LaterPurpose::Send => self.schedule_composer(at),
        }
    }

    // ---------------------------------------------------------- snooze

    /// Puts the conversations of `rows` away until `at`.
    pub(super) fn snooze(&mut self, rows: &[i64], at: i64) -> Task<Message> {
        if let Err(e) = self.store.snooze(rows, at) {
            return self.toast(format!("Erreur de la base locale : {e}"), true);
        }
        // The conversation read leaves the list (unless the list is
        // "Plus tard", where it only changes time).
        let reading_left = !self.snoozed
            && self
                .reading
                .as_ref()
                .is_some_and(|r| r.rows.iter().any(|row| rows.contains(row)));
        if reading_left {
            self.reading = None;
            self.selected = None;
        }
        self.clear_checked();
        self.reload_now();
        let when = later::describe_local(at, self.clock.now());
        self.toast(format!("Rappel {when}"), false)
    }

    /// "Annuler le rappel": the conversations come back now, in place.
    fn unsnooze(&mut self) -> Task<Message> {
        let rows = self.action_rows();
        if rows.is_empty() {
            return Task::none();
        }
        if let Err(e) = self.store.unsnooze(&rows) {
            return self.toast(format!("Erreur de la base locale : {e}"), true);
        }
        if self.snoozed {
            self.reading = None;
            self.selected = None;
        }
        self.clear_checked();
        self.reload_now();
        self.toast("Rappel annulé", false)
    }

    /// Whether the lines acted on are snoozed (the list is "Plus tard").
    pub fn selection_snoozed(&self) -> bool {
        self.snoozed && self.sender.is_none() && self.search_query.is_none()
    }

    /// What the timer does: wakes the conversations due (unread again,
    /// on top of their list, with a notification), then sends the
    /// scheduled mail due.
    fn tick(&mut self) -> Task<Message> {
        let (woken, unread) = self.wake_due();
        notify::show(self.reminders(&woken));
        let mut tasks = vec![unread];
        let due = self
            .store
            .due_scheduled(self.clock.now())
            .unwrap_or_default();
        for record in due {
            if !self.sending.contains(&record.id) {
                tasks.push(self.send_scheduled(record));
            }
        }
        Task::batch(tasks)
    }

    /// Wakes the snoozed conversations that are due: their newest message
    /// is marked unread, here and on the server.
    pub(super) fn wake_due(&mut self) -> (Vec<Woken>, Task<Message>) {
        let woken = match self.store.wake_snoozed(self.clock.now()) {
            Ok(woken) => woken,
            Err(e) => {
                eprintln!("Impossible de réveiller les rappels : {e}");
                return (Vec::new(), Task::none());
            }
        };
        if woken.is_empty() {
            return (woken, Task::none());
        }
        let rows: Vec<i64> = woken.iter().flat_map(|w| w.rows.iter().copied()).collect();
        let _ = self.store.set_seen(&rows, false);
        self.reload_now();
        let unread = self.remote_seen(&rows, false);
        (woken, unread)
    }

    /// The notifications of woken conversations (« Rappel : expéditeur »,
    /// then the subject), unless the user turned notifications off.
    pub(super) fn reminders(&self, woken: &[Woken]) -> Vec<Notice> {
        if !self.settings.notifications {
            return Vec::new();
        }
        woken
            .iter()
            .map(|w| {
                let sender = match w.from_name.as_deref().map(str::trim) {
                    Some(name) if !name.is_empty() => name.to_string(),
                    _ => w.from_addr.clone(),
                };
                Notice {
                    title: format!("Rappel : {sender}"),
                    body: match w.subject.trim() {
                        "" => "(sans objet)".to_string(),
                        subject => subject.to_string(),
                    },
                }
            })
            .collect()
    }

    // ------------------------------------------------------- scheduled

    /// The scheduled mail, read again for "Programmés".
    pub(super) fn refresh_outbox(&mut self) {
        let mut outbox = self.store.scheduled().unwrap_or_else(|e| {
            eprintln!("Impossible de lire les envois programmés : {e}");
            Vec::new()
        });
        // Only the active space's (the others still leave on time).
        outbox.retain(|record| self.in_space(record.fields.account));
        self.outbox = outbox;
    }

    /// "Envoyer plus tard": the message is checked now (recipients,
    /// size), then the time is asked.
    fn ask_send_later(&mut self) -> Task<Message> {
        let Some(composer) = &self.composer else {
            return Task::none();
        };
        if composer.sending {
            return Task::none();
        }
        let checked = self.account(composer.from).map(|account| {
            compose::build(compose::Draft {
                from_name: &account.name,
                from_addr: &account.email,
                to: &composer.to,
                cc: &composer.cc,
                bcc: &composer.bcc,
                subject: &composer.subject,
                body: String::new(),
                in_reply_to: None,
                references: None,
                attachments: &composer.attachments,
            })
        });
        match checked {
            Some(Ok(_)) => {
                self.open_picker(LaterPurpose::Send);
                Task::none()
            }
            Some(Err(error)) => {
                if let Some(composer) = &mut self.composer {
                    composer.error = Some(error);
                }
                Task::none()
            }
            None => Task::none(),
        }
    }

    /// Keeps the composer's message to send at `at`: saved here, closed,
    /// listed in "Programmés". A copy of it in the drafts folder goes.
    fn schedule_composer(&mut self, at: i64) -> Task<Message> {
        let id = match self.save_composer() {
            Ok(Some(id)) => id,
            Ok(None) => return Task::none(),
            Err(e) => return self.toast(e, true),
        };
        let Some(composer) = self.composer.take() else {
            return Task::none();
        };
        if let Err(e) = self.store.schedule_draft(id, at) {
            self.composer = Some(composer);
            return self.toast(format!("Erreur de la base locale : {e}"), true);
        }
        let mut tasks = Vec::new();
        if let Some(copy) = composer.server_copy {
            let _ = self.store.set_draft_copy(id, None, false);
            tasks.push(self.drop_server_draft(copy));
        }
        self.refresh_outbox();
        let when = later::describe_local(at, self.clock.now());
        tasks.push(self.toast(format!("Envoi programmé {when}"), false));
        Task::batch(tasks)
    }

    /// "Modifier": the scheduled mail goes back in the composer; it is
    /// not sent while there, and keeps its time when the composer closes.
    fn edit_scheduled(&mut self, id: i64) -> Task<Message> {
        if self.sending.contains(&id) {
            return Task::none();
        }
        let close = self.close_composer();
        if self.composer.is_some() {
            return close;
        }
        let Ok(Some(record)) = self.store.draft(id) else {
            return close;
        };
        let files = self.store.draft_files(id).unwrap_or_default();
        let _ = self.store.set_draft_open(id, true);
        self.composer = Some(super::Composer::from_draft(&record, files));
        self.screen = Screen::Mail;
        Task::batch([close, iced::widget::operation::focus(super::BODY_ID)])
    }

    /// Sends a scheduled mail now (its time came, or "Envoyer
    /// maintenant"). The answer comes back as `LaterMsg::Sent`.
    fn send_scheduled(&mut self, record: DraftRecord) -> Task<Message> {
        let id = record.id;
        let f = &record.fields;
        let Some(account) = self.account(f.account).cloned() else {
            return Task::none();
        };
        let attachments: Vec<Attached> = self
            .store
            .draft_files(id)
            .unwrap_or_default()
            .into_iter()
            .map(|file| Attached {
                name: file.name,
                mime: file.mime,
                data: Arc::new(file.data),
            })
            .collect();
        let built = compose::build(compose::Draft {
            from_name: &account.name,
            from_addr: &account.email,
            to: &f.to,
            cc: &f.cc,
            bcc: &f.bcc,
            subject: &f.subject,
            body: f.body.clone(),
            in_reply_to: f.in_reply_to.clone(),
            references: f.references.clone(),
            attachments: &attachments,
        });
        let outgoing = match built {
            Ok(outgoing) => outgoing,
            Err(error) => {
                let _ = self.store.scheduled_failed(id, &error);
                self.refresh_outbox();
                return Task::none();
            }
        };
        let _ = self.store.mark_send_attempt(id, self.clock.now());
        self.sending.insert(id);
        self.refresh_outbox();
        self.request(account.id, |reply| Job::Send {
            outgoing: Box::new(outgoing),
            reply,
        })
        .map(move |message| match message {
            Message::Worker(_, event) => Message::Later(LaterMsg::Sent(id, event)),
            other => other,
        })
    }

    /// A worker answered about scheduled mail `id`: sent (gone, or kept
    /// when late), or failed (kept with the reason).
    fn scheduled_answer(&mut self, id: i64, event: Event) -> Task<Message> {
        let result = match event {
            Event::Sent(result) => result,
            Event::Gone => Err("Le service de ce compte s'est arrêté ; redémarrez Avero".into()),
            _ => return Task::none(),
        };
        self.sending.remove(&id);
        let task = match result {
            Ok(warning) => {
                let _ = self.store.scheduled_sent(id, self.clock.now(), LATE_AFTER);
                let text = warning.unwrap_or_else(|| "Message programmé envoyé".to_string());
                self.toast(text, false)
            }
            Err(error) => {
                let _ = self.store.scheduled_failed(id, &error);
                self.toast(format!("Envoi programmé impossible : {error}"), true)
            }
        };
        self.refresh_outbox();
        task
    }
}

/// How a scheduled mail is doing, for "Programmés": its time, and when
/// it left or why it could not.
pub fn scheduled_status(record: &DraftRecord, now: i64, sending: bool) -> String {
    let planned = record
        .send_at
        .map(|at| later::describe_local(at, now))
        .unwrap_or_default();
    if let Some(sent) = record.sent_at {
        return format!(
            "Envoyé en retard {} (prévu {planned}) : Avero était fermé",
            later::describe_local(sent, now)
        );
    }
    if sending {
        return "Envoi en cours…".to_string();
    }
    match &record.error {
        Some(error) => format!("Échec de l'envoi prévu {planned} : {error}"),
        None if record.send_at.is_some_and(|at| at <= now) => {
            format!("Prévu {planned} : part dans un instant")
        }
        None => format!("Envoi prévu {planned}"),
    }
}
