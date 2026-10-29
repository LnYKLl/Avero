//! The composer, drawn in place of the reading pane.

use super::{hr, icon_button, icon_label_button};
use crate::app::{App, ComposeMsg, Composer, LaterMsg, Message};
use crate::compose::{human_size, total_size};
use crate::icons::{self, Icon, Tone};
use crate::theme::{self, MEDIUM, SEMIBOLD};
use avero_core::{Account, AccountId};
use iced::widget::text::Wrapping;
use iced::widget::{
    Column, Row, button, column, container, pick_list, row, space, text, text_editor, text_input,
};
use iced::{Alignment, Element, Fill, Padding, keyboard};
use std::fmt;

/// Comfortable writing width.
const MAX_WIDTH: f32 = 760.0;

/// An entry of the "De" picker.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sender {
    id: AccountId,
    label: String,
}

impl Sender {
    fn of(account: &Account) -> Sender {
        Sender {
            id: account.id,
            label: format!("{} <{}>", account.name, account.email),
        }
    }
}

impl fmt::Display for Sender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

pub fn view<'a>(app: &'a App, composer: &'a Composer) -> Element<'a, Message> {
    let title = if composer.send_at.is_some() {
        "Envoi programmé"
    } else if composer.in_reply_to.is_some() {
        "Répondre"
    } else if composer.subject.to_lowercase().starts_with("fwd:") {
        "Transférer"
    } else {
        "Nouveau message"
    };
    let top = row![
        text(title).size(16).font(SEMIBOLD),
        space().width(Fill),
        icon_button(
            Icon::Trash,
            "Supprimer le brouillon",
            Some(Message::Compose(ComposeMsg::Discard))
        ),
        icon_button(
            Icon::Close,
            "Fermer sans envoyer (Échap) : le brouillon est gardé",
            Some(Message::Compose(ComposeMsg::Cancel))
        ),
    ]
    .align_y(Alignment::Center)
    .padding([10, 20]);

    let senders: Vec<Sender> = app.accounts.iter().map(Sender::of).collect();
    let selected = senders.iter().find(|s| s.id == composer.from).cloned();
    let from = pick_list(senders, selected, |s: Sender| {
        Message::Compose(ComposeMsg::From(s.id))
    })
    .text_size(14)
    .padding([7, 2])
    .width(Fill)
    .style(theme::bare_pick_list)
    .menu_style(theme::menu);

    let mut to_line = Row::new().align_y(Alignment::Center).push(
        text_input("Destinataires, séparés par des virgules", &composer.to)
            .id(crate::app::TO_ID)
            .on_input(|v| Message::Compose(ComposeMsg::To(v)))
            .size(14)
            .padding([8, 2])
            .style(theme::bare_input),
    );
    if !composer.show_copies {
        to_line = to_line.push(
            button(text("Cc / Cci").size(13).font(MEDIUM))
                .padding([4, 8])
                .on_press(Message::Compose(ComposeMsg::ShowCopies))
                .style(theme::link),
        );
    }

    let mut fields = Column::new()
        .push(line("De", from))
        .push(hr())
        .push(line("À", to_line))
        .push(hr());
    if composer.show_copies {
        fields = fields
            .push(line("Cc", input("Copie", &composer.cc, ComposeMsg::Cc)))
            .push(hr())
            .push(line(
                "Cci",
                input("Copie cachée", &composer.bcc, ComposeMsg::Bcc),
            ))
            .push(hr());
    }
    fields = fields
        .push(line(
            "Objet",
            input("Objet du message", &composer.subject, ComposeMsg::Subject),
        ))
        .push(hr());

    let editor = text_editor(&composer.body)
        .id(crate::app::BODY_ID)
        .placeholder("Écrivez votre message…")
        .on_action(|action| Message::Compose(ComposeMsg::Edit(action)))
        .key_binding(|press| {
            let enter = matches!(
                press.key.as_ref(),
                keyboard::Key::Named(keyboard::key::Named::Enter)
            );
            if enter && press.modifiers.command() {
                Some(text_editor::Binding::Custom(Message::Compose(
                    ComposeMsg::Send,
                )))
            } else {
                text_editor::Binding::from_key_press(press)
            }
        })
        .size(14)
        .line_height(1.55)
        .padding(Padding::new(0.0).top(16))
        .height(Fill)
        .style(theme::bare_editor);

    let page = column![fields, editor].max_width(MAX_WIDTH);

    container(column![
        top,
        hr(),
        container(page)
            .padding(Padding::new(0.0).left(32).right(32))
            .height(Fill)
            .center_x(Fill),
        hr(),
        bottom(app, composer),
    ])
    .width(Fill)
    .height(Fill)
    .style(theme::page)
    .into()
}

/// One header line: a label then the field.
fn line<'a>(label: &'a str, field: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![
        container(text(label).size(13).style(theme::muted)).width(56),
        field.into(),
    ]
    .align_y(Alignment::Center)
    .padding([4, 0])
    .into()
}

fn input<'a>(
    placeholder: &'a str,
    value: &'a str,
    on_input: fn(String) -> ComposeMsg,
) -> Element<'a, Message> {
    text_input(placeholder, value)
        .on_input(move |v| Message::Compose(on_input(v)))
        .size(14)
        .padding([8, 2])
        .style(theme::bare_input)
        .into()
}

fn bottom<'a>(app: &'a App, composer: &'a Composer) -> Element<'a, Message> {
    let mut files = Row::new().spacing(8).align_y(Alignment::Center);
    for (index, attachment) in composer.attachments.iter().enumerate() {
        files = files.push(
            container(
                row![
                    icons::icon(Icon::Paperclip, 14.0, Tone::Muted),
                    text(super::ellipsize(&attachment.name, 26))
                        .size(12)
                        .font(MEDIUM)
                        .wrapping(Wrapping::None),
                    text(human_size(attachment.data.len()))
                        .size(11)
                        .style(theme::muted),
                    button(icons::icon(Icon::Close, 12.0, Tone::Muted))
                        .padding(3)
                        .on_press(Message::Compose(ComposeMsg::RemoveAttachment(index)))
                        .style(theme::ghost),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .padding(Padding {
                top: 4.0,
                bottom: 4.0,
                left: 10.0,
                right: 4.0,
            })
            .style(theme::attachment),
        );
    }
    let mut info = Column::new().spacing(6).width(Fill);
    if !composer.attachments.is_empty() {
        info = info.push(files.wrap());
        info = info.push(
            text(format!(
                "{} au total (25 Mo maximum)",
                human_size(total_size(&composer.attachments))
            ))
            .size(11)
            .style(theme::muted),
        );
    }
    let smtp_missing = app
        .account(composer.from)
        .is_some_and(|a| a.smtp.host.trim().is_empty());
    if smtp_missing && composer.error.is_none() {
        info = info.push(
            row![
                icons::icon(Icon::Alert, 14.0, Tone::Danger),
                text(crate::worker::SMTP_MISSING)
                    .size(12)
                    .style(theme::danger_text),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
    }
    if let Some(error) = &composer.error {
        info = info.push(
            row![
                icons::icon(Icon::Alert, 14.0, Tone::Danger),
                text(error.clone()).size(12).style(theme::danger_text),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
    } else if let Some(at) = composer.send_at {
        info = info.push(
            text(format!(
                "Programmé {} : fermer garde cette heure",
                crate::later::describe_local(at, app.clock.now())
            ))
            .size(11)
            .style(theme::muted),
        );
    } else if composer.draft_id.is_some() && !composer.dirty {
        info = info.push(
            text("Brouillon enregistré sur cet ordinateur")
                .size(11)
                .style(theme::muted),
        );
    }
    let idle = !composer.sending;
    let send_label = if composer.sending {
        "Envoi…"
    } else {
        "Envoyer"
    };
    let actions = row![
        icon_label_button(
            Icon::Paperclip,
            "Joindre",
            Tone::Text,
            idle.then_some(Message::Compose(ComposeMsg::Attach)),
            theme::ghost,
        ),
        button(text("Fermer").size(13).font(MEDIUM))
            .padding([8, 14])
            .on_press_maybe(idle.then_some(Message::Compose(ComposeMsg::Cancel)))
            .style(theme::secondary),
        icon_label_button(
            Icon::CalendarClock,
            "Envoyer plus tard",
            Tone::Text,
            idle.then_some(Message::Later(LaterMsg::SendLater)),
            theme::secondary,
        ),
        super::tip(
            icon_label_button(
                Icon::Send,
                send_label,
                Tone::OnAccent,
                idle.then_some(Message::Compose(ComposeMsg::Send)),
                theme::primary,
            ),
            "Ctrl+Entrée",
            iced::widget::tooltip::Position::Top,
        ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    row![info, actions]
        .spacing(16)
        .padding([12, 20])
        .align_y(Alignment::Center)
        .into()
}
