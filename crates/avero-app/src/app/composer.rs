//! The composer: writing a new message, a reply or a forward.

use super::{App, Message};
use crate::compose::{self, Attached, Draft, Fields, MAX_ATTACHMENTS};
use crate::worker::Job;
use avero_core::AccountId;
use iced::Task;
use iced::widget::text_editor;
use std::sync::Arc;

/// Id of the "À" field, focused for a new message.
pub const TO_ID: &str = "compose-to";
/// Id of the body editor, focused for a reply or a forward.
pub const BODY_ID: &str = "compose-body";

/// Messages of the composer.
#[derive(Debug, Clone)]
pub enum ComposeMsg {
    New,
    From(AccountId),
    To(String),
    Cc(String),
    Bcc(String),
    Subject(String),
    ShowCopies,
    Edit(text_editor::Action),
    Attach,
    Attached(Vec<(String, Arc<Vec<u8>>)>),
    RemoveAttachment(usize),
    Send,
    Cancel,
}

/// A draft being written.
pub struct Composer {
    pub from: AccountId,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: text_editor::Content,
    /// Cc and Cci lines are shown.
    pub show_copies: bool,
    pub attachments: Vec<Attached>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    /// The body the draft started from, without signature.
    base: String,
    /// The signature put in the body (swapped when the sending account
    /// changes, unless the user edited it).
    signature: String,
    pub sending: bool,
    pub error: Option<String>,
}

impl Composer {
    /// An empty message from `from`, signed with `signature`.
    pub fn new(from: AccountId, signature: &str) -> Composer {
        Composer::from_fields(from, Fields::default(), Vec::new(), signature)
    }

    /// A draft filled with `fields` (a reply or a forward); the signature
    /// goes above the quote.
    pub fn from_fields(
        from: AccountId,
        fields: Fields,
        attachments: Vec<Attached>,
        signature: &str,
    ) -> Composer {
        Composer {
            from,
            show_copies: !fields.cc.is_empty(),
            to: fields.to,
            cc: fields.cc,
            bcc: String::new(),
            subject: fields.subject,
            body: text_editor::Content::with_text(&compose::with_signature(
                &fields.body,
                signature,
            )),
            attachments,
            in_reply_to: fields.in_reply_to,
            references: fields.references,
            base: fields.body,
            signature: signature.to_string(),
            sending: false,
            error: None,
        }
    }

    /// Sends the draft from another account: its signature replaces the
    /// current one, unless the user edited the signature.
    pub fn change_sender(&mut self, from: AccountId, signature: &str) {
        self.from = from;
        if let Some(body) =
            compose::swap_signature(&self.body.text(), &self.base, &self.signature, signature)
        {
            self.body = text_editor::Content::with_text(&body);
            self.signature = signature.to_string();
        }
    }

    /// Adds files, refusing the ones that would go over the size limit.
    /// Returns the names refused.
    pub fn add_files(&mut self, files: Vec<(String, Arc<Vec<u8>>)>) -> Vec<String> {
        let mut refused = Vec::new();
        for (name, data) in files {
            if compose::total_size(&self.attachments) + data.len() > MAX_ATTACHMENTS {
                refused.push(name);
                continue;
            }
            self.attachments.push(Attached {
                mime: compose::mime_for(&name).to_string(),
                name,
                data,
            });
        }
        refused
    }
}

impl App {
    pub(super) fn update_composer(&mut self, message: ComposeMsg) -> Task<Message> {
        if let ComposeMsg::New = message {
            if self.composer.is_none()
                && let Some(from) = self.default_sender()
            {
                let signature = self.signature_of(from);
                self.composer = Some(Composer::new(from, &signature));
                self.screen = super::Screen::Mail;
                return iced::widget::operation::focus(TO_ID);
            }
            return Task::none();
        }
        let new_signature = match &message {
            ComposeMsg::From(id) => self.signature_of(*id),
            _ => String::new(),
        };
        let Some(composer) = &mut self.composer else {
            return Task::none();
        };
        if composer.sending && !matches!(message, ComposeMsg::Edit(_)) {
            return Task::none();
        }
        composer.error = None;
        match message {
            ComposeMsg::New => {}
            ComposeMsg::From(id) => composer.change_sender(id, &new_signature),
            ComposeMsg::To(text) => composer.to = text,
            ComposeMsg::Cc(text) => composer.cc = text,
            ComposeMsg::Bcc(text) => composer.bcc = text,
            ComposeMsg::Subject(text) => composer.subject = text,
            ComposeMsg::ShowCopies => composer.show_copies = true,
            ComposeMsg::Edit(action) => {
                if !composer.sending || !action.is_edit() {
                    composer.body.perform(action);
                }
            }
            ComposeMsg::Attach => {
                return Task::perform(pick_files(), ComposeMsg::Attached).map(Message::Compose);
            }
            ComposeMsg::Attached(files) => {
                let refused = composer.add_files(files);
                if !refused.is_empty() {
                    composer.error = Some(format!(
                        "Trop volumineux (25 Mo maximum au total) : {}",
                        refused.join(", ")
                    ));
                }
            }
            ComposeMsg::RemoveAttachment(index) => {
                if index < composer.attachments.len() {
                    composer.attachments.remove(index);
                }
            }
            ComposeMsg::Send => return self.send_draft(),
            ComposeMsg::Cancel => self.composer = None,
        }
        Task::none()
    }

    /// The signature of an account (empty when it has none).
    pub(super) fn signature_of(&self, account: AccountId) -> String {
        self.account(account)
            .map(|a| a.signature.clone())
            .unwrap_or_default()
    }

    fn send_draft(&mut self) -> Task<Message> {
        let Some(composer) = &self.composer else {
            return Task::none();
        };
        let Some(account) = self.account(composer.from) else {
            return Task::none();
        };
        if account.smtp.host.trim().is_empty() {
            if let Some(composer) = &mut self.composer {
                composer.error = Some(crate::worker::SMTP_MISSING.to_string());
            }
            return Task::none();
        }
        let built = compose::build(Draft {
            from_name: &account.name,
            from_addr: &account.email,
            to: &composer.to,
            cc: &composer.cc,
            bcc: &composer.bcc,
            subject: &composer.subject,
            body: composer.body.text(),
            in_reply_to: composer.in_reply_to.clone(),
            references: composer.references.clone(),
            attachments: &composer.attachments,
        });
        let from = composer.from;
        match built {
            Ok(outgoing) => {
                let task = self.request(from, |reply| Job::Send {
                    outgoing: Box::new(outgoing),
                    reply,
                });
                if let Some(composer) = &mut self.composer {
                    composer.sending = true;
                }
                task
            }
            Err(error) => {
                if let Some(composer) = &mut self.composer {
                    composer.error = Some(error);
                }
                Task::none()
            }
        }
    }

    /// A worker finished sending.
    pub(super) fn sent(&mut self, result: Result<Option<String>, String>) -> Task<Message> {
        match result {
            Ok(warning) => {
                self.composer = None;
                let text = warning.unwrap_or_else(|| "Message envoyé".to_string());
                self.toast(text, false)
            }
            Err(error) => {
                if let Some(composer) = &mut self.composer {
                    composer.sending = false;
                    composer.error = Some(error);
                }
                Task::none()
            }
        }
    }
}

/// Opens the system "Open" dialog and reads the chosen files.
async fn pick_files() -> Vec<(String, Arc<Vec<u8>>)> {
    let Some(handles) = rfd::AsyncFileDialog::new()
        .set_title("Joindre des fichiers")
        .pick_files()
        .await
    else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for handle in handles {
        let data = handle.read().await;
        files.push((handle.file_name(), Arc::new(data)));
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_over_the_limit_are_refused() {
        let mut composer = Composer::new(AccountId(1), "");
        let small = Arc::new(vec![0u8; 10]);
        let big = Arc::new(vec![0u8; MAX_ATTACHMENTS]);
        let refused = composer.add_files(vec![
            ("a.pdf".into(), small.clone()),
            ("gros.zip".into(), big),
            ("b.txt".into(), small),
        ]);
        assert_eq!(refused, vec!["gros.zip".to_string()]);
        assert_eq!(composer.attachments.len(), 2);
        assert_eq!(composer.attachments[0].mime, "application/pdf");
    }

    #[test]
    fn reply_fields_fill_the_composer() {
        let fields = Fields {
            to: "a@x.fr".into(),
            cc: "b@x.fr".into(),
            subject: "Re: x".into(),
            body: "\n\n> y".into(),
            in_reply_to: Some("<1@x>".into()),
            references: Some("<1@x>".into()),
        };
        let composer = Composer::from_fields(AccountId(2), fields, Vec::new(), "");
        assert!(composer.show_copies);
        assert_eq!(composer.body.text(), "\n\n> y");
        assert_eq!(composer.in_reply_to.as_deref(), Some("<1@x>"));
    }

    #[test]
    fn signature_follows_the_sender_until_edited() {
        let fields = Fields {
            body: "\n\n> y".into(),
            ..Fields::default()
        };
        let mut composer = Composer::from_fields(AccountId(1), fields, Vec::new(), "Marie");
        assert_eq!(composer.body.text(), "\n\n-- \nMarie\n\n> y");
        composer.change_sender(AccountId(2), "Paul\nPro");
        assert_eq!(composer.from, AccountId(2));
        assert_eq!(composer.body.text(), "\n\n-- \nPaul\nPro\n\n> y");
        // The user edits the signature: it stays from now on.
        composer.body = text_editor::Content::with_text("\n\n-- \nPaul (perso)\n\n> y");
        composer.change_sender(AccountId(1), "Marie");
        assert_eq!(composer.from, AccountId(1));
        assert_eq!(composer.body.text(), "\n\n-- \nPaul (perso)\n\n> y");
    }
}
