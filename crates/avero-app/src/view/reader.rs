//! The reading pane: actions ("Répondre", "Transférer", "Plus tard" as
//! text, the others as icons), the sorting bar (tab, unsubscribe), then
//! the subject and the messages in rounded cards: headers, attachments
//! and text. A conversation shows its messages one under the other,
//! oldest first; the folded ones take one line each. Under it, the quick
//! reply ("Répondre à tous…") in a card.

use super::{
    avatar, find_account, format_full_date, hr, icon_button, icon_label_button, tip, to_color,
};
use crate::app::{
    App, Body, Entry, LaterMsg, Message, QUICK_REPLY_ID, QuickMsg, Reading, tab_name,
};
use crate::compose::{Mode, human_size};
use crate::icons::{self, Icon, Tone};
use crate::theme::{self, BOLD, MEDIUM, SEMIBOLD};
use avero_core::{Category, UnifiedItem};
use avero_sync::parse::{Address, ParsedBody};
use chrono::Local;
use iced::widget::text::Wrapping;
use iced::widget::{
    Column, Row, button, center, column, container, mouse_area, row, scrollable, space, text,
    text_editor, tooltip,
};
use iced::{Alignment, Color, Element, Fill, Padding, keyboard};

/// Comfortable reading width.
const MAX_WIDTH: f32 = 760.0;

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
        spam_action(app, |icon, label, message| action(
            icon,
            label,
            Some(message)
        )),
        action(
            if all_starred(app) {
                Icon::StarFilled
            } else {
                Icon::Star
            },
            if all_starred(app) {
                "Retirer des favoris"
            } else {
                "Favoris"
            },
            Some(Message::ToggleStar)
        ),
        later_action(app, |icon, label, message| action(
            icon,
            label,
            Some(message)
        )),
    ]
    .spacing(8)
    .wrap();
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
        Body::Ready { content, marks, .. } => {
            let editor = text_editor(content)
                .highlight_with::<crate::marks::Highlighter>(marks.clone(), crate::marks::format)
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
            let editor = if fill { editor.height(Fill) } else { editor };
            // A link opens on the release (see `ReaderReleased`).
            mouse_area(editor)
                .on_release(Message::ReaderReleased)
                .into()
        }
    }
}

/// A single message, as before conversations: one card whose text fills
/// the pane.
fn message<'a>(app: &'a App, item: &'a UnifiedItem, reading: &'a Reading) -> Element<'a, Message> {
    let index = reading.focus;
    let entry = reading.focused();
    let parsed = parsed_of(entry);

    let mut card = Column::new()
        .spacing(14)
        .push(headers(app, &entry.item, parsed));
    if let Some(parsed) = parsed
        && !parsed.attachments.is_empty()
    {
        card = card.push(indented(attachments(index, parsed)));
    }
    card = card.push(indented(body_text(entry, index, true)));
    let page = column![
        title(item, None),
        container(card)
            .width(Fill)
            .height(Fill)
            .padding(Padding {
                top: 16.0,
                right: 18.0,
                bottom: 18.0,
                left: 18.0,
            })
            .style(theme::panel),
    ]
    .spacing(12)
    .max_width(MAX_WIDTH);

    let mut pane = column![action_bar(app, item, reading), hr()];
    if let Some(bar) = sorting_bar(app, reading) {
        pane = pane.push(bar).push(hr());
    }
    pane.push(
        container(page)
            .padding(Padding {
                top: 22.0,
                bottom: 0.0,
                left: 24.0,
                right: 24.0,
            })
            .height(Fill)
            .center_x(Fill),
    )
    .push(quick_reply(app, reading))
    .into()
}

/// The subject of what is read, with "3 messages" for a conversation.
fn title<'a>(item: &'a UnifiedItem, count: Option<usize>) -> Element<'a, Message> {
    // A long subject wraps beside the pill; a short one keeps it close.
    let subject = text(item.subject.clone())
        .size(21)
        .font(BOLD)
        .wrapping(Wrapping::WordOrGlyph);
    let subject = if item.subject.chars().count() > 34 {
        subject.width(Fill)
    } else {
        subject
    };
    let mut line = Row::new()
        .spacing(10)
        .align_y(Alignment::Center)
        .push(subject);
    if let Some(count) = count {
        line = line.push(
            container(text(format!("{count} messages")).size(12))
                .padding([2, 10])
                .style(theme::count_pill),
        );
    }
    container(line).padding(Padding::new(0.0).bottom(4)).into()
}

/// A conversation: its messages one under the other, in a scrolling page.
fn conversation<'a>(
    app: &'a App,
    item: &'a UnifiedItem,
    reading: &'a Reading,
) -> Element<'a, Message> {
    let mut page = Column::new()
        .spacing(12)
        .max_width(MAX_WIDTH)
        .push(title(item, Some(reading.entries.len())));
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

    let mut pane = column![action_bar(app, item, reading), hr()];
    if let Some(bar) = sorting_bar(app, reading) {
        pane = pane.push(bar).push(hr());
    }
    pane.push(
        scrollable(
            container(page)
                .padding(Padding {
                    top: 22.0,
                    bottom: 12.0,
                    left: 24.0,
                    right: 24.0,
                })
                .center_x(Fill),
        )
        .height(Fill)
        .style(theme::scrollbar),
    )
    .push(quick_reply(app, reading))
    .into()
}

/// Under the conversation, in a card: "Répondre à tous…", a field that
/// grows with its text (Ctrl+Entrée sends), "Envoyer", and the full
/// composer one click away with the text typed.
fn quick_reply<'a>(app: &'a App, reading: &'a Reading) -> Element<'a, Message> {
    let reply = &app.quick_reply;
    let ready = reading
        .entries
        .last()
        .is_some_and(|entry| matches!(entry.body, Body::Ready { .. }));
    let mut field = text_editor(&reply.content)
        .id(QUICK_REPLY_ID)
        .placeholder("Répondre à tous…")
        .key_binding(quick_reply_binding)
        .size(14)
        .line_height(1.45)
        .padding([8, 0])
        .min_height(36)
        .max_height(180)
        .wrapping(Wrapping::WordOrGlyph)
        .style(theme::bare_editor);
    // Without `on_action` the field is disabled: nothing changes while
    // the reply leaves.
    if !reply.sending {
        field = field.on_action(|action| Message::QuickReply(QuickMsg::Edit(action)));
    }
    let can_send = ready && !reply.sending && reply.has_text();
    let send_label = if reply.sending { "Envoi…" } else { "Envoyer" };
    let send = tip(
        button(text(send_label).size(14).font(BOLD))
            .height(38)
            .padding([9, 18])
            .on_press_maybe(can_send.then_some(Message::QuickReply(QuickMsg::Send)))
            .style(theme::primary),
        "Envoyer à tous (Ctrl+Entrée), sans citer la conversation",
        tooltip::Position::Top,
    );
    let expand = icon_button(
        Icon::Compose,
        "Ouvrir dans l'éditeur complet",
        (ready && !reply.sending).then_some(Message::QuickReply(QuickMsg::Expand)),
    );
    let card = container(
        row![
            icons::icon(Icon::ReplyAll, 16.0, Tone::Muted),
            container(field).width(Fill),
            expand,
            send,
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .padding(Padding {
        top: 7.0,
        right: 8.0,
        bottom: 7.0,
        left: 16.0,
    })
    .style(theme::panel);
    let mut bottom = Column::new().spacing(6).max_width(MAX_WIDTH).push(card);
    if let Some(error) = &reply.error {
        bottom = bottom.push(
            row![
                icons::icon(Icon::Alert, 14.0, Tone::Danger),
                text(error.clone()).size(12).style(theme::danger_text),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
    }
    container(bottom)
        .padding(Padding {
            top: 12.0,
            right: 24.0,
            bottom: 20.0,
            left: 24.0,
        })
        .center_x(Fill)
        .into()
}

/// The keys of the quick reply: Ctrl+Entrée sends, the others type
/// (a focused field keeps every letter, so the one-key shortcuts stay
/// off while typing in it).
pub fn quick_reply_binding(press: text_editor::KeyPress) -> Option<text_editor::Binding<Message>> {
    let focused = matches!(press.status, text_editor::Status::Focused { .. });
    let enter = matches!(
        press.key.as_ref(),
        keyboard::Key::Named(keyboard::key::Named::Enter)
    );
    if focused && enter && press.modifiers.command() {
        Some(text_editor::Binding::Custom(Message::QuickReply(
            QuickMsg::Send,
        )))
    } else {
        text_editor::Binding::from_key_press(press)
    }
}

/// Whether every line acted on is starred.
fn all_starred(app: &App) -> bool {
    let items = app.action_items();
    !items.is_empty() && items.iter().all(|item| item.flagged)
}

/// Under the actions: the tab of the conversation ("Classer dans", when
/// the inbox is sorted in tabs) and "Se désinscrire" when the mail offers
/// it. `None` when there is nothing to show.
fn sorting_bar<'a>(app: &'a App, reading: &'a Reading) -> Option<Element<'a, Message>> {
    let tabs = app.settings.inbox_tabs && !reading.sender.is_empty();
    if !tabs && reading.unsubscribe.is_none() {
        return None;
    }
    let mut bar = Row::new().spacing(6).align_y(Alignment::Center);
    if tabs {
        bar = bar.push(text("Classer dans").size(12).style(theme::muted));
        for category in Category::ALL {
            let selected = reading.category == Some(category);
            bar = bar.push(
                button(text(tab_name(category)).size(12).font(MEDIUM))
                    .padding([3, 9])
                    .on_press_maybe((!selected).then_some(Message::Reclassify(category)))
                    .style(theme::chip(selected)),
            );
        }
    }
    bar = bar.push(space().width(Fill));
    if let Some(offer) = &reading.unsubscribe {
        bar = bar.push(if offer.requested {
            container(
                row![
                    icons::icon(Icon::Check, 14.0, Tone::Muted),
                    text("Désinscription demandée").size(12).style(theme::muted),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .padding([4, 6])
            .into()
        } else {
            tip(
                icon_label_button(
                    Icon::MailX,
                    "Se désinscrire",
                    Tone::Text,
                    Some(Message::Unsubscribe),
                    theme::secondary,
                )
                .padding([4, 10]),
                format!("Ne plus recevoir les messages de {}", offer.sender_name),
                tooltip::Position::Bottom,
            )
        });
    }
    Some(container(bar).padding([6, 20]).into())
}

/// Who wrote a message: its sender's name ("Moi" for the user), the
/// address, and the avatar's colour (stable per sender; the brand colour
/// for the user).
fn author(app: &App, item: &UnifiedItem, parsed: Option<&ParsedBody>) -> (String, String, Color) {
    let (name, address) = match parsed.and_then(|p| p.from.as_ref()) {
        Some(Address {
            name: Some(name),
            addr,
        }) => (name.clone(), addr.clone()),
        Some(Address { addr, .. }) => (addr.clone(), addr.clone()),
        None => (item.from_display.clone(), item.from_addr.clone()),
    };
    let own = app
        .accounts
        .iter()
        .any(|a| a.email.eq_ignore_ascii_case(address.trim()));
    if own {
        return (name, address, app.tokens().brand);
    }
    let color = super::avatar_color(if address.is_empty() { &name } else { &address });
    (name, address, color)
}

/// A folded message: avatar, sender, start of the text and time on one
/// line, in a bordered card.
fn folded<'a>(app: &'a App, index: usize, entry: &'a Entry) -> Element<'a, Message> {
    let item = &entry.item;
    let unread = !item.seen;
    let (name, _, color) = author(app, item, parsed_of(entry));
    let preview = if item.snippet.is_empty() {
        String::new()
    } else {
        super::ellipsize(&item.snippet, 90)
    };
    let line = row![
        avatar(&name, color, 30.0),
        text(super::ellipsize(&name, 28))
            .size(14)
            .font(if unread { BOLD } else { SEMIBOLD })
            .wrapping(Wrapping::None),
        container(
            text(preview)
                .size(14)
                .style(theme::muted)
                .wrapping(Wrapping::None)
        )
        .width(Fill)
        .clip(true),
        text(super::format_date(item.date, Local::now()))
            .size(12)
            .style(theme::muted),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    tip(
        button(line)
            .width(Fill)
            .padding([12, 16])
            .on_press(Message::EntryToggled(index))
            .style(theme::folded_card),
        "Afficher ce message",
        tooltip::Position::Top,
    )
}

/// Under the headers, the text and attachments start at the name.
fn indented<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    container(content)
        .padding(Padding::new(0.0).left(AVATAR + 12.0))
        .into()
}

/// An unfolded message of a conversation, in a card: its headers (a
/// click folds it), attachments and text. `focused` marks the one Reply
/// answers.
fn unfolded<'a>(
    app: &'a App,
    index: usize,
    entry: &'a Entry,
    focused: bool,
) -> Element<'a, Message> {
    let parsed = parsed_of(entry);
    let header = button(headers(app, &entry.item, parsed))
        .width(Fill)
        .padding(0)
        .on_press(Message::EntryToggled(index))
        .style(theme::ghost);
    let mut card = Column::new().spacing(14).push(header);
    if let Some(parsed) = parsed
        && !parsed.attachments.is_empty()
    {
        card = card.push(indented(attachments(index, parsed)));
    }
    card = card.push(indented(body_text(entry, index, false)));
    if focused {
        card = card.push(indented(
            text("Répondre et Transférer partent de ce message.")
                .size(11)
                .style(theme::muted)
                .into(),
        ));
    }
    container(card)
        .width(Fill)
        .padding(Padding {
            top: 16.0,
            right: 18.0,
            bottom: 18.0,
            left: 18.0,
        })
        .style(theme::panel)
        .into()
}

/// A text button of the action bar ("Répondre", "Transférer", "Plus
/// tard"): its icon and label in the muted colour.
fn text_action<'a>(
    icon: Icon,
    label: &'a str,
    hint: String,
    on_press: Option<Message>,
    compact: bool,
) -> Element<'a, Message> {
    if compact {
        return icon_button(icon, hint, on_press);
    }
    let tone = if on_press.is_some() {
        Tone::Muted
    } else {
        Tone::Fixed(theme::alpha(theme::LIGHT.muted, 0.5))
    };
    tip(
        button(
            row![
                icons::icon(icon, 16.0, tone),
                text(label).size(14).font(MEDIUM).style(theme::muted),
            ]
            .spacing(7)
            .align_y(Alignment::Center),
        )
        .height(36)
        .padding([8, 12])
        .on_press_maybe(on_press)
        .style(theme::ghost),
        hint,
        tooltip::Position::Bottom,
    )
}

/// Below these widths of the pane, the action bar shows "Plus tard",
/// then all its actions, as icons only.
const LABELS_BELOW: f32 = 700.0;
const ICONS_BELOW: f32 = 560.0;

/// The width of the reading pane in this window.
fn pane_width(app: &App) -> f32 {
    super::card_width(app) - super::list::width(app)
}

/// "Répondre", "Transférer", "Plus tard" as text, then the other actions
/// as icons.
fn action_bar<'a>(
    app: &'a App,
    _item: &'a UnifiedItem,
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
    // A narrower pane keeps only the icons: of "Plus tard" first, then
    // of all three.
    let width = pane_width(app);
    let compact = width < ICONS_BELOW;
    let later = later_action(app, |icon, label, message| {
        let compact = width < LABELS_BELOW;
        text_action(icon, label, format!("{label} (H)"), Some(message), compact)
    });
    let bar = row![
        text_action(
            Icon::Reply,
            "Répondre",
            "Répondre (R)".to_string(),
            when_ready(Message::Answer(Mode::Reply)),
            compact
        ),
        text_action(
            Icon::Forward,
            "Transférer",
            "Transférer (F)".to_string(),
            when_ready(Message::Answer(Mode::Forward)),
            compact
        ),
        later,
        space().width(Fill),
        icon_button(
            Icon::ReplyAll,
            "Répondre à tous (A)",
            when_ready(Message::Answer(Mode::ReplyAll))
        ),
        star_button(app),
        icon_button(
            Icon::Archive,
            archive_label,
            app.can_archive().then_some(Message::Archive)
        ),
        spam_action(app, |icon, label, message| icon_button(
            icon,
            format!("{label} (!)"),
            Some(message)
        )),
        icon_button(
            Icon::Mail,
            "Marquer comme non lu (U)",
            Some(Message::MarkUnread)
        ),
        icon_button(
            Icon::Trash,
            delete_label,
            app.can_delete().then_some(Message::Delete)
        ),
    ]
    .spacing(4)
    .align_y(Alignment::Center);
    container(bar)
        .height(60)
        .padding([0, 16])
        .center_y(60)
        .into()
}

/// "Spam", or "Pas spam" when the selection is in a spam folder, drawn by
/// `draw` (an icon button or an icon with its label).
fn spam_action<'a, E>(app: &App, draw: impl Fn(Icon, &'static str, Message) -> E) -> E
where
    E: Into<Element<'a, Message>>,
{
    if app.selection_in_junk() {
        draw(Icon::Inbox, "Pas spam", Message::MarkSpam(false))
    } else {
        draw(Icon::Shield, "Spam", Message::MarkSpam(true))
    }
}

/// "Plus tard", or "Annuler le rappel" in the "Plus tard" list, drawn by
/// `draw`.
fn later_action<'a, E>(app: &App, draw: impl Fn(Icon, &'static str, Message) -> E) -> E
where
    E: Into<Element<'a, Message>>,
{
    if app.selection_snoozed() {
        draw(
            Icon::AlarmClock,
            "Annuler le rappel",
            Message::Later(LaterMsg::Unsnooze),
        )
    } else {
        draw(
            Icon::AlarmClock,
            "Plus tard",
            Message::Later(LaterMsg::Snooze),
        )
    }
}

/// The star of the action bar: filled (and coloured) when starred.
fn star_button(app: &App) -> Element<'_, Message> {
    let starred = all_starred(app);
    let (icon, tone, label) = if starred {
        (
            Icon::StarFilled,
            Tone::Fixed(theme::STAR),
            "Retirer des favoris (S)",
        )
    } else {
        (Icon::Star, Tone::Text, "Ajouter aux favoris (S)")
    };
    tip(
        button(icons::icon(icon, 18.0, tone))
            .padding(8)
            .on_press(Message::ToggleStar)
            .style(theme::ghost),
        label,
        tooltip::Position::Bottom,
    )
}

/// Size of the avatar of an unfolded message.
const AVATAR: f32 = 40.0;

/// Avatar, sender (a click shows all their mail), "à …" and time of a
/// message.
fn headers<'a>(
    app: &'a App,
    item: &'a UnifiedItem,
    parsed: Option<&'a ParsedBody>,
) -> Element<'a, Message> {
    let (name, address, color) = author(app, item, parsed);
    let mut recipients = Vec::new();
    if let Some(parsed) = parsed {
        if !parsed.to.is_empty() {
            recipients.push(format!("à {}", names(&parsed.to)));
        }
        if !parsed.cc.is_empty() {
            recipients.push(format!("cc {}", names(&parsed.cc)));
        }
    }
    let shown_address = if address == name {
        String::new()
    } else {
        address.clone()
    };
    let sender = tip(
        button(
            row![
                text(name.clone()).size(14).font(BOLD),
                text(shown_address).size(12).style(theme::muted),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding(0)
        .on_press(Message::ShowSender(address, name.clone()))
        .style(theme::link_plain),
        format!("Tous les mails de {name}"),
        tooltip::Position::Bottom,
    );
    let mut who = Column::new().spacing(2).width(Fill).push(sender);
    if !recipients.is_empty() {
        who = who.push(text(recipients.join(" · ")).size(12).style(theme::muted));
    }
    let date = parsed
        .map(|p| p.date)
        .filter(|&d| d > 0)
        .unwrap_or(item.date);
    row![
        avatar(&name, color, AVATAR),
        who,
        tip(
            text(super::format_when(date, Local::now()))
                .size(12)
                .style(theme::muted),
            format_full_date(date),
            tooltip::Position::Bottom,
        ),
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
