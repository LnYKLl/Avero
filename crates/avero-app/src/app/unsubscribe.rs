//! "Se désinscrire" in the reader: what the mail offers (read when it is
//! opened), the confirmation, then the one-click request, the mail or the
//! page in the browser. Once asked, "Désinscription demandée" is
//! remembered for the sender (locally).

use super::{App, Message, Overlay};
use crate::unsubscribe::{self, Method};
use crate::worker::{Event, Job};
use avero_core::AccountId;
use avero_sync::smtp::{Outgoing, Recipient};
use iced::Task;

/// What the reader offers to unsubscribe from a mailing list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsubscribeOffer {
    /// The sender's address (what "Désinscription demandée" is
    /// remembered for).
    pub sender: String,
    /// The sender's name, for the dialog.
    pub sender_name: String,
    /// The account that received the list: a mail is sent from it.
    pub account: AccountId,
    pub method: Method,
    /// Unsubscribing from this sender was already asked.
    pub requested: bool,
}

impl App {
    /// What `row` offers to unsubscribe, from its stored headers.
    pub(super) fn unsubscribe_offer(
        &self,
        row: i64,
        sender: &str,
        sender_name: &str,
        account: Option<AccountId>,
    ) -> Option<UnsubscribeOffer> {
        let headers = self.store.headers(row).ok()??;
        let method = unsubscribe::choose(
            headers.list_unsubscribe.as_deref()?,
            headers.list_unsubscribe_post.as_deref(),
        )?;
        Some(UnsubscribeOffer {
            sender: sender.to_string(),
            sender_name: sender_name.to_string(),
            account: account?,
            method,
            requested: matches!(self.store.unsubscribed_at(sender), Ok(Some(_))),
        })
    }

    pub(super) fn update_unsubscribe(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Unsubscribe => {
                if let Some(offer) = self.reading.as_ref().and_then(|r| r.unsubscribe.clone()) {
                    self.overlay = Some(Overlay::ConfirmUnsubscribe(offer));
                }
                Task::none()
            }
            Message::ConfirmUnsubscribe => {
                let Some(Overlay::ConfirmUnsubscribe(offer)) = self.overlay.take() else {
                    return Task::none();
                };
                self.unsubscribe(offer)
            }
            Message::Unsubscribed(sender, result) => match result {
                Ok(()) => self.unsubscribe_asked(&sender),
                Err(error) => self.toast(format!("Désinscription impossible : {error}"), true),
            },
            _ => Task::none(),
        }
    }

    /// Does what the confirmed offer says. In demo mode nothing leaves the
    /// machine: the request is only remembered.
    fn unsubscribe(&mut self, offer: UnsubscribeOffer) -> Task<Message> {
        let sender = offer.sender.clone();
        if self.is_demo() {
            let task = self.unsubscribe_asked(&sender);
            let toast = self.toast("Mode démo : aucune demande n'a été envoyée", false);
            return Task::batch([task, toast]);
        }
        match offer.method {
            Method::OneClick(url) => Task::perform(unsubscribe::one_click(url), move |result| {
                Message::Unsubscribed(sender.clone(), result)
            }),
            Method::Mail { to, subject, body } => {
                let Some(account) = self.account(offer.account) else {
                    return self.toast("Le compte qui a reçu ce message n'existe plus", true);
                };
                // Only what the link asks for: no quote, no signature.
                let outgoing = Outgoing {
                    from_name: Some(account.name.clone()).filter(|n| !n.trim().is_empty()),
                    from_addr: account.email.clone(),
                    to: vec![Recipient::new(None, &to)],
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject,
                    body,
                    in_reply_to: None,
                    references: None,
                    attachments: Vec::new(),
                };
                self.request(offer.account, |reply| Job::Send {
                    outgoing: Box::new(outgoing),
                    reply,
                })
                .map(move |message| match message {
                    Message::Worker(_, Event::Sent(result)) => {
                        Message::Unsubscribed(sender.clone(), result.map(|_| ()))
                    }
                    Message::Worker(_, Event::Gone) => Message::Unsubscribed(
                        sender.clone(),
                        Err("le service de ce compte s'est arrêté".to_string()),
                    ),
                    other => other,
                })
            }
            // The browser does the rest: it counts as asked.
            Method::Web(url) | Method::InsecureWeb(url) => {
                let open = self.update(Message::OpenUrl(url.to_string()));
                Task::batch([open, self.unsubscribe_asked(&sender)])
            }
        }
    }

    /// Remembers that unsubscribing from `sender` was asked, and says so.
    fn unsubscribe_asked(&mut self, sender: &str) -> Task<Message> {
        let now = chrono::Local::now().timestamp();
        if let Err(e) = self.store.mark_unsubscribed(sender, now) {
            return self.toast(format!("Erreur de la base locale : {e}"), true);
        }
        if let Some(offer) = self
            .reading
            .as_mut()
            .and_then(|r| r.unsubscribe.as_mut())
            .filter(|offer| offer.sender == sender)
        {
            offer.requested = true;
        }
        self.toast("Désinscription demandée", false)
    }
}
