//! The reading pane: actions, headers, attachments and the text. A
//! conversation shows its messages one under the other, oldest first; the
//! folded ones take one line each.

use super::{
    avatar, find_account, format_full_date, hr, icon_button, icon_label_button, tip, to_color,
};
use crate::app::{App, Body, Entry, Message, Reading};
use crate::compose::{Mode, human_size};
use crate::icons::{self, Icon, Tone};
use crate::theme::{self, MEDIUM, SEMIBOLD};
use avero_core::UnifiedItem;
use avero_sync::parse::{Address, ParsedBody};
use chrono::Local;
use iced::widget::text::Wrapping;
use iced::widget::{
    Column, Row, button, center, column, container, row, scrollable, space, text, text_editor,
    tooltip,
};
use iced::{Alignment, Color, Element, Fill, Padding};

/// Comfortable reading width.
const MAX_WIDTH: f32 = 720.0;

pub fn view(app: &App) -> Element<'_, Message> {
    let content = match (app.selected_item(), &app.reading) {
        _ if !app.checked.is_empty() => checked(app),
        (Some(item), Some(reading)) if reading.is_thread() => conversation(app, item, reading),
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
            text("J et K pour passer d'un message à l'autre, ? pour tous les raccourcis")
                .size(12)
                .style(theme::muted),
        ]
        .spacing(12)
        .align_x(Alignment::Center),
    )
    .into()
}

/// Several lines checked: how many, and what can be done to all of them
/// (one job per account).
fn checked(app: &App) -> Element<'_, Message> {
    let count = app.checked.len();
    let mut accounts = Row::new().spacing(6).align_y(Alignment::Center);
    for id in app.action_accounts() {
        if let Some(account) = find_account(&app.accounts, id) {
            accounts = accounts.push(
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
            );
        }
    }
    let delete_label = if app.selection_in_trash() {
        "Supprimer définitivement"
    } else {
        "Supprimer"
    };
    let action = |icon, label, message: Option<Message>| {
        icon_label_button(icon, label, Tone::Text, message, theme::secondary)
    };
    let actions = row![
        action(
            Icon::Archive,
            "Archiver",
            app.can_archive().then_some(Message::Archive)
        ),
        action(
            Icon::Trash,
            delete_label,
            app.can_delete().then_some(Message::Delete)
        ),
        action(Icon::MailOpen, "Marquer lu", Some(Message::MarkRead)),
        action(Icon::Mail, "Marquer non lu", Some(Message::MarkUnread)),
    ]
    .spacing(8);
    let mut page = column![
        icons::icon(Icon::Check, 40.0, Tone::Accent),
        text(super::list::selection_label(count, app.lines_are_threads()))
            .size(20)
            .font(SEMIBOLD),
        accounts,
        actions,
    ]
    .spacing(16)
    .align_x(Alignment::Center);
    if let Some(why) = app.archive_problem() {
        page = page.push(
            text(format!("Archiver : {}", why.to_lowercase()))
                .size(12)
                .style(theme::muted),
        );
    }
    page = page.push(
        button(text("Tout désélectionner (Échap)").size(13).font(MEDIUM))
            .padding([6, 10])
            .on_press(Message::SelectAll(false))
            .style(theme::link),
    );
    center(page).padding(24).into()
}

/// The parsed message of an entry, once its body is ready.
fn parsed_of(entry: &Entry) -> Option<&ParsedBody> {
    match &entry.body {
        Body::Ready { opened, .. } => Some(&opened.parsed),
        _ => None,
    }
}

/// The text of entry `index`; `fill` lets it take the pane's height (and
/// scroll by itself) instead of its own.
fn body_text<'a>(entry: &'a Entry, index: usize, fill: bool) -> Element<'a, Message> {
    match &entry.body {
        Body::NotLoaded | Body::Loading => notice(Icon::Loader, "Chargement du message…", false),
        Body::Failed(error) => notice(Icon::Alert, error, true),
        Body::Ready { content, .. } => {
            let editor = text_editor(content)
                .on_action(move |action| Message::ReaderAction(index, action))
                // Read-only: only the Ctrl keys (copy, select all) stay
                // here, the others go to the shortcuts (J, K, E…).
                .key_binding(|press| {
                    if press.modifiers.command() {
                        text_editor::Binding::from_key_press(press)
                    } else {
                        None
                    }
                })
                .size(14)
                .line_height(1.55)
                .padding(0)
                .wrapping(Wrapping::WordOrGlyph)
                .style(theme::bare_editor);
            if fill {
                editor.height(Fill).into()
            } else {
                editor.into()
            }
        }
    }
}

/// A single message, as before conversations: its text fills the pane.
fn message<'a>(app: &'a App, item: &'a UnifiedItem, reading: &'a Reading) -> Element<'a, Message> {
    let index = reading.focus;
    let entry = reading.focused();
    let parsed = parsed_of(entry);

    let mut page = Column::new()
        .spacing(18)
        .max_width(MAX_WIDTH)
        .push(text(item.subject.clone()).size(22).font(SEMIBOLD))
        .push(headers(app, &entry.item, parsed, 40.0));
    if let Some(parsed) = parsed
        && !parsed.attachments.is_empty()
    {
        page = page.push(attachments(index, parsed));
    }
    page = page.push(hr()).push(body_text(entry, index, true));

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

/// A conversation: its messages one under the other, in a scrolling page.
fn conversation<'a>(
    app: &'a App,
    item: &'a UnifiedItem,
    reading: &'a Reading,
) -> Element<'a, Message> {
    let mut page = Column::new().spacing(10).max_width(MAX_WIDTH).push(
        column![
            text(item.subject.clone()).size(22).font(SEMIBOLD),
            text(format!("{} messages", reading.entries.len()))
                .size(12)
                .style(theme::muted),
        ]
        .spacing(4)
        .padding(Padding::new(0.0).bottom(8)),
    );
    for (index, entry) in reading.entries.iter().enumerate() {
        page = page.push(if entry.expanded {
            unfolded(
                app,
                index,
                entry,
                reading.is_thread() && index == reading.focus,
            )
        } else {
            folded(app, index, entry)
        });
    }

    column![
        action_bar(app, item, reading),
        hr(),
        scrollable(
            container(page)
                .padding(Padding {
                    top: 24.0,
                    bottom: 24.0,
                    left: 32.0,
                    right: 32.0,
                })
                .center_x(Fill),
        )
        .height(Fill)
        .style(theme::scrollbar),
    ]
    .into()
}

/// Colour of the first account that holds the message.
fn entry_color(app: &App, item: &UnifiedItem) -> Color {
    item.accounts
        .first()
        .and_then(|id| find_account(&app.accounts, *id))
        .map(|a| to_color(a.color))
        .unwrap_or(app.tokens().accent)
}

/// A folded message: sender, start of the text and date on one line.
fn folded<'a>(app: &'a App, index: usize, entry: &'a Entry) -> Element<'a, Message> {
    let item = &entry.item;
    let unread = !item.seen;
    let preview = if item.snippet.is_empty() {
        String::new()
    } else {
        super::ellipsize(&item.snippet, 60)
    };
    let line = row![
        avatar(&item.from_display, entry_color(app, item), 28.0),
        text(super::ellipsize(&item.from_display, 24))
            .size(13)
            .font(if unread { SEMIBOLD } else { MEDIUM })
            .wrapping(Wrapping::None),
        container(
            text(preview)
                .size(12)
                .style(theme::muted)
                .wrapping(Wrapping::None)
        )
        .width(Fill)
        .clip(true),
        text(super::format_date(item.date, Local::now()))
            .size(12)
            .style(theme::muted),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    tip(
        button(line)
            .width(Fill)
            .padding([10, 14])
            .on_press(Message::EntryToggled(index))
            .style(theme::list_row(false)),
        "Afficher ce message",
        tooltip::Position::Top,
    )
}

/// An unfolded message of a conversation: its headers (a click folds it),
/// attachments and text. `focused` marks the one Reply answers.
fn unfolded<'a>(
    app: &'a App,
    index: usize,
    entry: &'a Entry,
    focused: bool,
) -> Element<'a, Message> {
    let parsed = parsed_of(entry);
    let header = button(headers(app, &entry.item, parsed, 32.0))
        .width(Fill)
        .padding(0)
        .on_press(Message::EntryToggled(index))
        .style(theme::ghost);
    let mut card = Column::new().spacing(14).push(header);
    if let Some(parsed) = parsed
        && !parsed.attachments.is_empty()
    {
        card = card.push(attachments(index, parsed));
    }
    card = card.push(body_text(entry, index, false));
    if focused {
        card = card.push(
            text("Répondre et Transférer partent de ce message.")
                .size(11)
                .style(theme::muted),
        );
    }
    container(card)
        .width(Fill)
        .padding([16, 18])
        .style(theme::panel)
        .into()
}

fn action_bar<'a>(
    app: &'a App,
    item: &'a UnifiedItem,
    reading: &'a Reading,
) -> Element<'a, Message> {
    let ready = matches!(reading.body(), Body::Ready { .. });
    let when_ready = |message: Message| ready.then_some(message);
    let whole = if reading.is_thread() {
        " la conversation"
    } else {
        ""
    };
    let archive_label = match app.archive_problem() {
        None => format!("Archiver{whole} (E)"),
        Some(why) => format!("Archiver ({})", why.to_lowercase()),
    };
    let delete_label = match app.delete_problem() {
        None if app.selection_in_trash() => {
            format!("Supprimer définitivement{whole} (Suppr)")
        }
        None => format!("Supprimer{whole} (Suppr)"),
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
            "Répondre (R)",
            when_ready(Message::Answer(Mode::Reply))
        ),
        icon_button(
            Icon::ReplyAll,
            "Répondre à tous (A)",
            when_ready(Message::Answer(Mode::ReplyAll))
        ),
        icon_button(
            Icon::Forward,
            "Transférer (F)",
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
            "Marquer comme non lu (U)",
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

/// Sender (avatar, name, address), recipients and date of a message.
fn headers<'a>(
    app: &'a App,
    item: &'a UnifiedItem,
    parsed: Option<&'a ParsedBody>,
    avatar_size: f32,
) -> Element<'a, Message> {
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
    row![
        avatar(&name, entry_color(app, item), avatar_size),
        who,
        text(format_full_date(date)).size(12).style(theme::muted),
    ]
    .spacing(12)
    .align_y(Alignment::Center)
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

/// The attachments of reader entry `entry`, each saved on a click.
fn attachments<'a>(entry: usize, parsed: &'a ParsedBody) -> Element<'a, Message> {
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
            .on_press(Message::SaveAttachment(entry, index))
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
