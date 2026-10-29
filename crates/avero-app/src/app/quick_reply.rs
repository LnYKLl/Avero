//! The quick reply: "Répondre à tous…" under the conversation read. What
//! is typed goes to everyone of the newest message, from the account that
//! received it, with its signature and no quote; the full composer is one
//! click away and takes the text along.

use super::{App, Body, Message};
use crate::compose::{self, Mode};
use crate::worker::{Event, Job};
use avero_core::AccountId;
use avero_sync::smtp::Outgoing;
use iced::Task;
use iced::widget::text_editor;

/// Id of the quick reply field.
pub const QUICK_REPLY_ID: &str = "quick-reply";

/// The quick reply field and how its sending goes.
#[derive(Default)]
pub struct QuickReply {
    pub content: text_editor::Content,
    pub sending: bool,
    pub error: Option<String>,
}

impl QuickReply {
    /// Whether something was typed.
    pub fn has_text(&self) -> bool {
        !self.content.text().trim().is_empty()
    }
}

/// Messages of the quick reply.
#[derive(Debug, Clone)]
pub enum QuickMsg {
    Edit(text_editor::Action),
    /// "Envoyer" or Ctrl+Entrée.
    Send,
    /// Opens the full composer with the text typed.
    Expand,
    /// The worker sent the reply typed under conversation `.0` (see
    /// `App::quick_generation`), with a warning when the Sent copy
    /// failed, or could not.
    Sent(u64, Result<Option<String>, String>),
}

impl App {
    pub(super) fn update_quick_reply(&mut self, message: QuickMsg) -> Task<Message> {
        match message {
            QuickMsg::Edit(action) => {
                if !(self.quick_reply.sending && action.is_edit()) {
                    if action.is_edit() {
                        self.quick_reply.error = None;
                    }
                    self.quick_reply.content.perform(action);
                }
                Task::none()
            }
            QuickMsg::Send => self.send_quick_reply(),
            QuickMsg::Expand => {
                if self.quick_reply.sending {
                    return Task::none();
                }
                let typed = self.quick_reply.content.text();
                let Some(index) = self.reading.as_ref().map(|r| r.entries.len() - 1) else {
                    return Task::none();
                };
                let task = self.open_answer(index, Mode::ReplyAll, typed.trim_end());
                if self.composer.is_some() {
                    self.quick_reply = QuickReply::default();
                }
                task
            }
            // Another conversation was opened meanwhile: its field stays
            // as it is, a notice tells how it went.
            QuickMsg::Sent(generation, result) if generation != self.quick_generation => {
                match result {
                    Ok(warning) => self.toast(
                        warning.unwrap_or_else(|| "Réponse envoyée".to_string()),
                        false,
                    ),
                    Err(error) => self.toast(format!("Réponse non envoyée : {error}"), true),
                }
            }
            QuickMsg::Sent(_, result) => {
                self.quick_reply.sending = false;
                match result {
                    Ok(warning) => {
                        self.quick_reply = QuickReply::default();
                        let text = warning.unwrap_or_else(|| "Réponse envoyée".to_string());
                        self.toast(text, false)
                    }
                    Err(error) => {
                        self.quick_reply.error = Some(error);
                        Task::none()
                    }
                }
            }
        }
    }

    /// The reply to send for what was typed: the account it leaves from
    /// and the message, or what is wrong.
    pub(super) fn quick_reply_message(&self) -> Result<(AccountId, Outgoing), String> {
        let reading = self.reading.as_ref().ok_or("Aucune conversation ouverte")?;
        let newest = reading
            .entries
            .last()
            .ok_or("Aucune conversation ouverte")?;
        let Body::Ready { opened, .. } = &newest.body else {
            return Err("Le dernier message n'est pas encore chargé".to_string());
        };
        let from = newest
            .item
            .accounts
            .first()
            .or(reading.accounts.first())
            .copied()
            .or_else(|| self.default_sender())
            .ok_or("Aucun compte pour répondre")?;
        let account = self.account(from).ok_or("Aucun compte pour répondre")?;
        if account.smtp.host.trim().is_empty() {
            return Err(crate::worker::SMTP_MISSING.to_string());
        }
        let own: Vec<String> = self.accounts.iter().map(|a| a.email.clone()).collect();
        let fields = compose::quick_reply(
            &opened.parsed,
            &own,
            &self.quick_reply.content.text(),
            &account.signature,
        );
        let outgoing = compose::build(compose::Draft {
            from_name: &account.name,
            from_addr: &account.email,
            to: &fields.to,
            cc: &fields.cc,
            bcc: "",
            subject: &fields.subject,
            body: fields.body,
            in_reply_to: fields.in_reply_to,
            references: fields.references,
            attachments: &[],
        })?;
        Ok((from, outgoing))
    }

    fn send_quick_reply(&mut self) -> Task<Message> {
        if self.quick_reply.sending || !self.quick_reply.has_text() {
            return Task::none();
        }
        let (from, outgoing) = match self.quick_reply_message() {
            Ok(message) => message,
            Err(error) => {
                self.quick_reply.error = Some(error);
                return Task::none();
            }
        };
        self.quick_reply.sending = true;
        self.quick_reply.error = None;
        let generation = self.quick_generation;
        self.request(from, |reply| Job::Send {
            outgoing: Box::new(outgoing),
            reply,
        })
        .map(move |message| match message {
            Message::Worker(_, Event::Sent(result)) => {
                Message::QuickReply(QuickMsg::Sent(generation, result))
            }
            Message::Worker(_, Event::Gone) => Message::QuickReply(QuickMsg::Sent(
                generation,
                Err("Le service de ce compte s'est arrêté ; redémarrez Avero".to_string()),
            )),
            other => other,
        })
    }
}
