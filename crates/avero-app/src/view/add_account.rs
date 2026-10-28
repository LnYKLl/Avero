//! The "add an IMAP account" form, shown in place of the reading pane.

use super::bold;
use crate::app::Message;
use avero_core::{AuthKind, NewAccount, Security, ServerConfig};
use iced::widget::{Column, button, column, row, text, text_input};
use iced::{Element, Fill, Theme};
use std::collections::HashMap;

/// The fields of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Name,
    Email,
    Host,
    Port,
    Username,
    Password,
}

/// What the user has typed so far, and what is wrong with it.
#[derive(Debug, Clone)]
pub struct AddAccountForm {
    pub name: String,
    pub email: String,
    pub host: String,
    pub port: String,
    pub username: String,
    pub password: String,
    /// Once the user edits the username, it stops following the e-mail.
    username_edited: bool,
    pub errors: HashMap<Field, String>,
}

impl AddAccountForm {
    pub fn new() -> AddAccountForm {
        AddAccountForm {
            name: String::new(),
            email: String::new(),
            host: String::new(),
            port: "993".to_string(),
            username: String::new(),
            password: String::new(),
            username_edited: false,
            errors: HashMap::new(),
        }
    }

    /// Stores a new value for one field and clears that field's error.
    pub fn set(&mut self, field: Field, value: String) {
        self.errors.remove(&field);
        match field {
            Field::Name => self.name = value,
            Field::Email => {
                if !self.username_edited {
                    self.username = value.clone();
                }
                self.email = value;
            }
            Field::Host => self.host = value,
            Field::Port => self.port = value,
            Field::Username => {
                self.username_edited = true;
                self.username = value;
            }
            Field::Password => self.password = value,
        }
    }

    /// Checks every field. On success returns the account to create and
    /// the password (which goes to the keyring, never to the database);
    /// otherwise records one message per wrong field and returns `None`.
    pub fn validate(&mut self) -> Option<(NewAccount, String)> {
        self.errors.clear();
        let email = self.email.trim().to_string();
        let host = self.host.trim().to_string();

        if !email.contains('@') {
            self.errors
                .insert(Field::Email, "Adresse e-mail invalide".to_string());
        }
        if host.is_empty() {
            self.errors
                .insert(Field::Host, "Serveur requis".to_string());
        }
        // `parse::<u16>()` already rejects negatives and anything above 65535.
        let port = match self.port.trim().parse::<u16>() {
            Ok(port) if port >= 1 => port,
            _ => {
                self.errors
                    .insert(Field::Port, "Port invalide (1 à 65535)".to_string());
                0
            }
        };
        if self.password.is_empty() {
            self.errors
                .insert(Field::Password, "Mot de passe requis".to_string());
        }
        if !self.errors.is_empty() {
            return None;
        }

        let name = match self.name.trim() {
            "" => email.clone(),
            name => name.to_string(),
        };
        let username = match self.username.trim() {
            "" => email.clone(),
            username => username.to_string(),
        };
        let smtp = ServerConfig {
            host: smtp_host_for(&host),
            port: 465,
            username: username.clone(),
            security: Security::Tls,
        };
        let account = NewAccount {
            name,
            email,
            imap: ServerConfig {
                host,
                port,
                username,
                security: Security::Tls,
            },
            smtp,
            auth: AuthKind::Password,
        };
        Some((account, self.password.clone()))
    }
}

/// Guesses the SMTP server from the IMAP one: `imap.x.com` gives
/// `smtp.x.com`, any other host is used as is.
fn smtp_host_for(imap_host: &str) -> String {
    match imap_host.strip_prefix("imap.") {
        Some(rest) => format!("smtp.{rest}"),
        None => imap_host.to_string(),
    }
}

pub fn view(form: &AddAccountForm) -> Element<'_, Message> {
    let field = |label: &'static str, field: Field, value: &str, secure: bool| {
        let input = text_input(label, value)
            .on_input(move |v| Message::AddAccountField(field, v))
            .secure(secure)
            .padding(6);
        let mut cell = Column::new()
            .push(text(label).size(13))
            .push(input)
            .spacing(3);
        if let Some(error) = form.errors.get(&field) {
            cell = cell.push(
                text(error.clone())
                    .size(12)
                    .style(|theme: &Theme| text::Style {
                        color: Some(theme.palette().danger),
                    }),
            );
        }
        cell
    };

    column![
        text("Ajouter un compte").size(22).font(bold()),
        field("Nom", Field::Name, &form.name, false),
        field("Adresse e-mail", Field::Email, &form.email, false),
        field("Serveur IMAP", Field::Host, &form.host, false),
        field("Port", Field::Port, &form.port, false),
        field("Identifiant", Field::Username, &form.username, false),
        field("Mot de passe", Field::Password, &form.password, true),
        row![
            button(text("Annuler").size(14))
                .on_press(Message::AddAccountCancelled)
                .style(button::secondary),
            button(text("Ajouter").size(14)).on_press(Message::AddAccountSubmitted),
        ]
        .spacing(8),
    ]
    .spacing(10)
    .padding(20)
    .max_width(420)
    .width(Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smtp_host_follows_imap_host() {
        assert_eq!(smtp_host_for("imap.example.com"), "smtp.example.com");
        assert_eq!(smtp_host_for("mail.example.com"), "mail.example.com");
    }
}
