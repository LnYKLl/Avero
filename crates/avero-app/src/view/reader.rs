//! The reading pane: actions, headers, attachments and the text.

use super::{avatar, find_account, format_full_date, hr, icon_button, tip, to_color};
use crate::app::{App, Body, Message, Reading};
use crate::compose::{Mode, human_size};
use crate::icons::{self, Icon, Tone};
use crate::theme::{self, MEDIUM, SEMIBOLD};
use avero_core::UnifiedItem;
use avero_sync::parse::{Address, ParsedBody};
use iced::widget::text::Wrapping;
use iced::widget::{
    Column, Row, button, center, column, container, row, space, text, text_editor, tooltip,
};
use iced::{Alignment, Element, Fill, Padding};

/// Comfortable reading width.
const MAX_WIDTH: f32 = 720.0;

pub fn view(app: &App) -> Element<'_, Message> {
    let content = match (app.selected_item(), &app.reading) {
        (Some(item), Some(reading)) => message(app, item, reading),
        _ => empty(),
    };
    container(content)
        .width(Fill)
        .height(Fill)
        .style(theme::page)
        .into()
}

fn empty<'a>() -> Element<'a, Message> {
    center(
        column![
            icons::icon(
                Icon::Mark,
                44.0,
                Tone::Fixed(theme::alpha(theme::LIGHT.muted, 0.45))
            ),
            text("Sélectionnez un message")
                .size(15)
                .font(MEDIUM)
                .style(theme::muted),
            text("J et K pour passer d'un message à l'autre")
                .size(12)
                .style(theme::muted),
        ]
        .spacing(12)
        .align_x(Alignment::Center),
    )
    .into()
}

fn message<'a>(app: &'a App, item: &'a UnifiedItem, reading: &'a Reading) -> Element<'a, Message> {
    let parsed = match &reading.body {
        Body::Ready { opened, .. } => Some(&opened.parsed),
        _ => None,
    };
    let body: Element<'a, Message> = match &reading.body {
        Body::Loading => notice(Icon::Loader, "Chargement du message…", false),
        Body::Failed(error) => notice(Icon::Alert, error, true),
        Body::Ready { content, .. } => text_editor(content)
            .on_action(Message::ReaderAction)
            .size(14)
            .line_height(1.55)
            .padding(0)
            .height(Fill)
            .wrapping(Wrapping::WordOrGlyph)
            .style(theme::bare_editor)
            .into(),
    };

    let mut page = Column::new()
        .spacing(18)
        .max_width(MAX_WIDTH)
        .push(headers(app, item, parsed));
    if let Some(parsed) = parsed
        && !parsed.attachments.is_empty()
    {
        page = page.push(attachments(parsed));
    }
    page = page.push(hr()).push(body);

    column![
        action_bar(app, item, reading),
        hr(),
        container(page)
            .padding(Padding {
                top: 24.0,
                bottom: 8.0,
                left: 32.0,
                right: 32.0,
            })
            .height(Fill)
            .center_x(Fill),
    ]
    .into()
}

fn action_bar<'a>(
    app: &'a App,
    item: &'a UnifiedItem,
    reading: &'a Reading,
) -> Element<'a, Message> {
    let ready = matches!(reading.body, Body::Ready { .. });
    let when_ready = |message: Message| ready.then_some(message);
    let archive_label = match app.archive_problem() {
        None => "Archiver".to_string(),
        Some(why) => format!("Archiver ({})", why.to_lowercase()),
    };
    let delete_label = match app.delete_problem() {
        None if app.selection_in_trash() => "Supprimer définitivement (Suppr)".to_string(),
        None => "Supprimer (Suppr)".to_string(),
        Some(why) => format!("Supprimer ({})", why.to_lowercase()),
    };
    let mut accounts = Row::new().spacing(6).align_y(Alignment::Center);
    for id in &item.accounts {
        if let Some(account) = find_account(&app.accounts, *id) {
            accounts = accounts.push(tip(
                container(
                    row![
                        super::dot(to_color(account.color), 8.0),
                        text(account.name.clone()).size(12).font(MEDIUM),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center),
                )
                .padding([4, 10])
                .style(theme::note),
                format!("Reçu sur {}", account.email),
                tooltip::Position::Bottom,
            ));
        }
    }
    row![
        icon_button(
            Icon::Reply,
            "Répondre (Ctrl+R)",
            when_ready(Message::Answer(Mode::Reply))
        ),
        icon_button(
            Icon::ReplyAll,
            "Répondre à tous",
            when_ready(Message::Answer(Mode::ReplyAll))
        ),
        icon_button(
            Icon::Forward,
            "Transférer",
            when_ready(Message::Answer(Mode::Forward))
        ),
        separator(),
        icon_button(
            Icon::Archive,
            archive_label,
            app.can_archive().then_some(Message::Archive)
        ),
        icon_button(
            Icon::Trash,
            delete_label,
            app.can_delete().then_some(Message::Delete)
        ),
        icon_button(
            Icon::Mail,
            "Marquer comme non lu",
            Some(Message::MarkUnread)
        ),
        space().width(Fill),
        accounts,
    ]
    .spacing(2)
    .padding([10, 20])
    .align_y(Alignment::Center)
    .into()
}

fn separator<'a>() -> Element<'a, Message> {
    container(super::vr()).height(20).padding([0, 6]).into()
}

fn headers<'a>(
    app: &'a App,
    item: &'a UnifiedItem,
    parsed: Option<&'a ParsedBody>,
) -> Element<'a, Message> {
    let color = item
        .accounts
        .first()
        .and_then(|id| find_account(&app.accounts, *id))
        .map(|a| to_color(a.color))
        .unwrap_or(app.tokens().accent);
    let (name, address) = match parsed.and_then(|p| p.from.as_ref()) {
        Some(Address {
            name: Some(name),
            addr,
        }) => (name.clone(), addr.clone()),
        Some(Address { addr, .. }) => (addr.clone(), String::new()),
        None => (item.from_display.clone(), item.from_addr.clone()),
    };
    let address = if address == name {
        String::new()
    } else {
        address
    };
    let mut recipients = Vec::new();
    if let Some(parsed) = parsed {
        if !parsed.to.is_empty() {
            recipients.push(format!("à {}", names(&parsed.to)));
        }
        if !parsed.cc.is_empty() {
            recipients.push(format!("cc {}", names(&parsed.cc)));
        }
    }
    let mut who = Column::new().spacing(3).width(Fill).push(
        row![
            text(name.clone()).size(14).font(SEMIBOLD),
            text(address).size(12).style(theme::muted),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    );
    if !recipients.is_empty() {
        who = who.push(text(recipients.join(" · ")).size(12).style(theme::muted));
    }
    let date = parsed
        .map(|p| p.date)
        .filter(|&d| d > 0)
        .unwrap_or(item.date);
    column![
        text(item.subject.clone()).size(22).font(SEMIBOLD),
        row![
            avatar(&name, color, 40.0),
            who,
            text(format_full_date(date)).size(12).style(theme::muted),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    ]
    .spacing(18)
    .into()
}

/// "Marie, Paul et 3 autres".
fn names(list: &[Address]) -> String {
    let shown: Vec<String> = list
        .iter()
        .take(3)
        .map(|a| a.name.clone().unwrap_or_else(|| a.addr.clone()))
        .collect();
    let rest = list.len().saturating_sub(3);
    if rest > 0 {
        format!(
            "{} et {rest} autre{}",
            shown.join(", "),
            if rest > 1 { "s" } else { "" }
        )
    } else {
        shown.join(", ")
    }
}

fn attachments<'a>(parsed: &'a ParsedBody) -> Element<'a, Message> {
    let chips: Vec<Element<'a, Message>> = parsed
        .attachments
        .iter()
        .enumerate()
        .map(|(index, attachment)| {
            let chip = button(
                row![
                    icons::icon(Icon::FileText, 18.0, Tone::Accent),
                    column![
                        text(super::ellipsize(&attachment.name, 34))
                            .size(13)
                            .font(MEDIUM)
                            .wrapping(Wrapping::None),
                        text(human_size(attachment.size))
                            .size(11)
                            .style(theme::muted),
                    ]
                    .spacing(1),
                    icons::icon(Icon::Download, 15.0, Tone::Muted),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .padding([8, 12])
            .on_press(Message::SaveAttachment(index))
            .style(theme::secondary);
            tip(chip, "Enregistrer sous…", tooltip::Position::Bottom)
        })
        .collect();
    Row::with_children(chips).spacing(8).wrap().into()
}

fn notice<'a>(icon: Icon, message: &str, error: bool) -> Element<'a, Message> {
    let tone = if error { Tone::Danger } else { Tone::Muted };
    container(
        row![
            icons::icon(icon, 16.0, tone),
            text(message.to_string()).size(13).style(theme::muted),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([12, 0])
    .into()
}
