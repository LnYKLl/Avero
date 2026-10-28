//! The list column: title, search, account chips, folders, account
//! problems, then the messages.

use super::{dot, ellipsize, find_account, format_date, hr, shared_avatar, tip, to_color};
use crate::app::{AccountMsg, App, Message, PAGE, SEARCH_ID};
use crate::icons::{self, Icon, Tone};
use crate::theme::{self, MEDIUM, SEMIBOLD};
use crate::worker::ErrorKind;
use avero_core::{Account, AccountFilter, AuthKind, FolderRole, UnifiedItem};
use chrono::{DateTime, Local};
use iced::widget::text::Wrapping;
use iced::widget::{
    Column, Row, button, column, container, row, scrollable, stack, text, text_input, tooltip,
};
use iced::{Alignment, Element, Fill, Padding};

/// Width of the list column, in pixels.
pub const WIDTH: f32 = 380.0;

/// The folders, in the order shown.
pub const FOLDERS: [(FolderRole, &str, Icon); 6] = [
    (FolderRole::Inbox, "Réception", Icon::Inbox),
    (FolderRole::Sent, "Envoyés", Icon::Send),
    (FolderRole::Drafts, "Brouillons", Icon::FileText),
    (FolderRole::Archive, "Archives", Icon::Archive),
    (FolderRole::Junk, "Spam", Icon::Shield),
    (FolderRole::Trash, "Corbeille", Icon::Trash),
];

/// French name of a folder role.
pub fn folder_name(role: FolderRole) -> &'static str {
    FOLDERS
        .iter()
        .find(|(r, _, _)| *r == role)
        .map_or("Dossier", |(_, name, _)| name)
}

pub fn view(app: &App) -> Element<'_, Message> {
    let mut header = Column::new()
        .push(title_row(app))
        .push(search(app))
        .push(account_chips(app))
        .push(folders(app))
        .spacing(14)
        .padding(Padding::new(20.0).bottom(14));
    for banner in banners(app) {
        header = header.push(banner);
    }

    let content = column![header, hr(), messages(app)];
    row![
        container(content)
            .width(WIDTH)
            .height(Fill)
            .style(theme::list_pane),
        super::vr(),
    ]
    .into()
}

fn title_row(app: &App) -> Element<'_, Message> {
    let title = match &app.search_query {
        Some(_) => "Recherche".to_string(),
        None => folder_name(app.folder).to_string(),
    };
    let scope = match app.filter {
        AccountFilter::All => "Tous les comptes".to_string(),
        AccountFilter::Only(id) => find_account(&app.accounts, id)
            .map(|a| a.email.clone())
            .unwrap_or_default(),
    };
    let syncing = app.is_syncing();
    let refresh_label = if syncing {
        "Synchronisation en cours…"
    } else {
        "Actualiser"
    };
    let refresh = tip(
        button(icons::icon_rotated(
            Icon::Refresh,
            18.0,
            if syncing { Tone::Accent } else { Tone::Muted },
            if syncing { app.spinner } else { 0.0 },
        ))
        .padding(8)
        .on_press_maybe((!syncing).then_some(Message::Refresh))
        .style(theme::ghost),
        refresh_label,
        tooltip::Position::Bottom,
    );
    row![
        column![
            text(title).size(22).font(SEMIBOLD),
            text(scope).size(12).style(theme::muted),
        ]
        .spacing(2)
        .width(Fill),
        refresh,
    ]
    .align_y(Alignment::Center)
    .into()
}

fn search(app: &App) -> Element<'_, Message> {
    let input = text_input("Rechercher dans les messages", &app.search_text)
        .id(SEARCH_ID)
        .on_input(Message::SearchChanged)
        .on_submit(Message::SearchSubmitted)
        .padding(Padding {
            top: 9.0,
            bottom: 9.0,
            left: 36.0,
            right: 36.0,
        })
        .size(14)
        .style(theme::search_input);
    let icon = container(icons::icon(Icon::Search, 16.0, Tone::Muted))
        .padding(Padding::new(0.0).left(12))
        .height(38)
        .center_y(38);
    let mut layers = vec![input.into(), icon.into()];
    if !app.search_text.is_empty() {
        let clear = button(icons::icon(Icon::Close, 14.0, Tone::Muted))
            .padding(5)
            .on_press(Message::SearchCleared)
            .style(theme::ghost);
        layers.push(
            container(clear)
                .width(Fill)
                .height(38)
                .align_x(Alignment::End)
                .align_y(Alignment::Center)
                .padding(Padding::new(0.0).right(6))
                .into(),
        );
    }
    stack(layers).width(Fill).into()
}

fn account_chips(app: &App) -> Element<'_, Message> {
    let total: usize = app.unread.values().sum();
    let mut chips = Row::new().spacing(5).push(chip(
        None,
        "Tous",
        total,
        app.filter == AccountFilter::All,
        AccountFilter::All,
    ));
    for account in &app.accounts {
        let only = AccountFilter::Only(account.id);
        let count = app.unread.get(&account.id).copied().unwrap_or(0);
        let chip = chip(
            Some(account),
            &account.name,
            count,
            app.filter == only,
            only,
        );
        chips = chips.push(tip(chip, account.email.clone(), tooltip::Position::Bottom));
    }
    scrollable(chips)
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new().width(0).scroller_width(0),
        ))
        .into()
}

fn chip<'a>(
    account: Option<&Account>,
    label: &str,
    unread: usize,
    selected: bool,
    filter: AccountFilter,
) -> Element<'a, Message> {
    let mut content = Row::new().spacing(6).align_y(Alignment::Center);
    if let Some(account) = account {
        content = content.push(dot(to_color(account.color), 8.0));
    }
    content = content.push(text(label.to_string()).size(13).font(MEDIUM));
    if unread > 0 {
        content = content.push(text(unread.to_string()).size(12).style(theme::muted));
    }
    button(content)
        .padding([5, 10])
        .on_press(Message::FilterSelected(filter))
        .style(theme::chip(selected))
        .into()
}

fn folders(app: &App) -> Element<'_, Message> {
    let mut tabs = Row::new().spacing(2).align_y(Alignment::Center);
    for (role, name, icon) in FOLDERS {
        let selected = app.search_query.is_none() && app.folder == role;
        let tone = if selected { Tone::Accent } else { Tone::Muted };
        let mut content = Row::new()
            .spacing(6)
            .align_y(Alignment::Center)
            .push(icons::icon(icon, 16.0, tone));
        if selected {
            content = content.push(text(name).size(13).font(MEDIUM));
            let unread: usize = app.unread.values().sum();
            if role == FolderRole::Inbox && unread > 0 {
                content = content.push(
                    container(text(unread.to_string()).size(11).font(SEMIBOLD))
                        .padding([1, 6])
                        .style(theme::badge),
                );
            }
        }
        let tab = button(content)
            .padding([6, 9])
            .on_press(Message::FolderSelected(role))
            .style(theme::chip(selected));
        if selected {
            tabs = tabs.push(tab);
        } else {
            tabs = tabs.push(tip(tab, name, tooltip::Position::Bottom));
        }
    }
    tabs.into()
}

/// One banner per account in trouble, with the action that fixes it.
fn banners(app: &App) -> Vec<Element<'_, Message>> {
    let mut banners = Vec::new();
    for account in &app.accounts {
        let Some(error) = app.status.get(&account.id).and_then(|s| s.error.as_ref()) else {
            continue;
        };
        let oauth = account.auth != AuthKind::Password;
        let (label, action) = match error.kind {
            ErrorKind::Reconnect => (
                "Reconnecter",
                Message::Account(AccountMsg::Reconnect(account.id)),
            ),
            ErrorKind::Auth if oauth => (
                "Reconnecter",
                Message::Account(AccountMsg::Reconnect(account.id)),
            ),
            ErrorKind::Auth => (
                "Corriger le mot de passe",
                Message::Account(AccountMsg::OpenPassword(account.id)),
            ),
            ErrorKind::Network | ErrorKind::Other => ("Réessayer", Message::Refresh),
        };
        let banner = container(
            column![
                row![
                    icons::icon(Icon::Alert, 16.0, Tone::Danger),
                    text(account.name.clone()).size(13).font(SEMIBOLD),
                    text(account.email.clone()).size(12).style(theme::muted),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                text(error.message.clone()).size(12),
                button(text(label).size(12).font(MEDIUM))
                    .padding([5, 10])
                    .on_press(action)
                    .style(theme::secondary),
            ]
            .spacing(8),
        )
        .padding(12)
        .width(Fill)
        .style(theme::error_banner);
        banners.push(banner.into());
    }
    banners
}

fn messages(app: &App) -> Element<'_, Message> {
    if app.items.is_empty() {
        return empty(app);
    }
    let now = Local::now();
    let selected = app.selected_index();
    let mut lines = Column::new().spacing(2).padding([8, 8]);
    for (index, item) in app.items.iter().enumerate() {
        lines = lines.push(message_row(app, index, item, selected == Some(index), now));
    }
    // A full last page means there may be more.
    if app.items.len().is_multiple_of(PAGE) {
        lines = lines.push(
            container(
                button(text("Charger plus").size(13).font(MEDIUM))
                    .padding([8, 16])
                    .on_press(Message::LoadMore)
                    .style(theme::secondary),
            )
            .padding(12)
            .center_x(Fill),
        );
    }
    scrollable(lines)
        .height(Fill)
        .style(theme::scrollbar)
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::new()
                .width(6)
                .scroller_width(6)
                .margin(2),
        ))
        .into()
}

fn empty(app: &App) -> Element<'_, Message> {
    let (title, detail) = match &app.search_query {
        Some(query) => (
            format!("Aucun résultat pour « {query} »"),
            "La recherche porte sur l'objet, l'expéditeur et le texte des messages déjà téléchargés.",
        ),
        None if app.is_syncing() => (
            "Synchronisation…".to_string(),
            "Les messages apparaissent dès qu'ils arrivent.",
        ),
        None => ("Aucun message".to_string(), "Ce dossier est vide."),
    };
    container(
        column![
            icons::icon(Icon::Inbox, 32.0, Tone::Muted),
            text(title).size(15).font(SEMIBOLD),
            text(detail)
                .size(13)
                .style(theme::muted)
                .align_x(Alignment::Center),
        ]
        .spacing(10)
        .align_x(Alignment::Center)
        .max_width(280),
    )
    .center_x(Fill)
    .padding(Padding::new(48.0).left(24).right(24))
    .into()
}

fn message_row<'a>(
    app: &'a App,
    index: usize,
    item: &'a UnifiedItem,
    is_selected: bool,
    now: DateTime<Local>,
) -> Element<'a, Message> {
    let unread = !item.seen;
    let colors: Vec<_> = item
        .accounts
        .iter()
        .filter_map(|id| find_account(&app.accounts, *id))
        .map(|a| to_color(a.color))
        .collect();

    let sender = text(ellipsize(&item.from_display, 30))
        .size(14)
        .font(if unread { SEMIBOLD } else { MEDIUM })
        .wrapping(Wrapping::None);
    let mut meta = Row::new().spacing(6).align_y(Alignment::Center);
    if item.has_attachments {
        meta = meta.push(icons::icon(Icon::Paperclip, 13.0, Tone::Muted));
    }
    let date_style = if unread { theme::accent } else { theme::muted };
    meta = meta.push(
        text(format_date(item.date, now))
            .size(12)
            .font(if unread { SEMIBOLD } else { theme::INTER })
            .style(date_style),
    );
    let first = row![container(sender).width(Fill).clip(true), meta]
        .spacing(8)
        .align_y(Alignment::Center);

    let subject = text(ellipsize(&item.subject, 40))
        .size(13)
        .font(if unread { MEDIUM } else { theme::INTER })
        .wrapping(Wrapping::None);
    let mut second = Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(container(subject).width(Fill).clip(true));
    if unread {
        second = second.push(super::dot(app.tokens().accent, 8.0));
    }

    let mut lines = Column::new()
        .spacing(3)
        .width(Fill)
        .push(first)
        .push(second);
    if !item.snippet.is_empty() {
        lines = lines.push(
            container(
                text(ellipsize(&item.snippet, 46))
                    .size(12)
                    .style(theme::muted)
                    .wrapping(Wrapping::None),
            )
            .clip(true),
        );
    }

    let content = row![shared_avatar(&item.from_display, &colors, 36.0), lines,]
        .spacing(12)
        .align_y(Alignment::Start);

    button(content)
        .width(Fill)
        .padding([10, 12])
        .on_press(Message::MessageSelected(index))
        .style(theme::list_row(is_selected))
        .into()
}
