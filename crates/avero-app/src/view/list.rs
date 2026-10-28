//! The list column: title, search, account chips, folders, the tabs of
//! the inbox, account problems, then the messages.

use super::{dot, ellipsize, find_account, format_date, hr, shared_avatar, tip, to_color};
use crate::app::{AccountMsg, App, Message, PAGE, SEARCH_ID, tab_name};
use crate::icons::{self, Icon, Tone};
use crate::theme::{self, MEDIUM, SEMIBOLD};
use crate::worker::ErrorKind;
use avero_core::{Account, AccountFilter, AuthKind, Category, FolderRole, UnifiedItem};
use chrono::{DateTime, Local};
use iced::widget::text::Wrapping;
use iced::widget::{
    Column, Row, button, checkbox, column, container, row, scrollable, stack, text, text_input,
    tooltip,
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
    if app.tabs_shown() {
        header = header.push(tabs(app));
    }
    for banner in banners(app) {
        header = header.push(banner);
    }

    let mut content = column![header, hr()];
    if !app.items.is_empty() {
        content = content.push(select_bar(app)).push(hr());
    }
    let content = content.push(messages(app));
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
    let title = match (&app.search_query, &app.sender) {
        (Some(_), _) => "Recherche".to_string(),
        (None, Some(sender)) => format!("Mails de {}", ellipsize(&sender.name, 24)),
        (None, None) if app.starred => "Favoris".to_string(),
        (None, None) => folder_name(app.folder).to_string(),
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
    let mut line = Row::new().spacing(4).align_y(Alignment::Center);
    // "Mails de X" goes back to what was shown before.
    if app.sender.is_some() && app.search_query.is_none() {
        line = line.push(tip(
            button(icons::icon(Icon::ChevronLeft, 20.0, Tone::Text))
                .padding(6)
                .on_press(Message::SenderBack)
                .style(theme::ghost),
            "Retour (Échap)",
            tooltip::Position::Bottom,
        ));
    }
    let scope = match &app.sender {
        Some(sender) if app.search_query.is_none() && sender.addr != sender.name => {
            format!("{} · {scope}", sender.addr)
        }
        _ => scope,
    };
    line.push(
        column![
            text(title).size(22).font(SEMIBOLD).wrapping(Wrapping::None),
            text(scope).size(12).style(theme::muted),
        ]
        .spacing(2)
        .width(Fill),
    )
    .push(refresh)
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
        // Gmail's archive is "All Mail", which is not synced: no view.
        if role == FolderRole::Archive && !app.archive_view && app.folder != role {
            continue;
        }
        let selected = app.in_folder() && app.folder == role;
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
            .padding(if selected { [6, 9] } else { [6, 7] })
            .on_press(Message::FolderSelected(role))
            .style(theme::chip(selected));
        if selected {
            tabs = tabs.push(tab);
        } else {
            tabs = tabs.push(tip(tab, name, tooltip::Position::Bottom));
        }
        // "Favoris" right after the inbox.
        if role == FolderRole::Inbox {
            tabs = tabs.push(starred_button(app));
        }
    }
    tabs.into()
}

/// "Favoris": the starred mail of every folder.
fn starred_button(app: &App) -> Element<'_, Message> {
    let selected = app.starred && app.sender.is_none() && app.search_query.is_none();
    let mut content = Row::new()
        .spacing(6)
        .align_y(Alignment::Center)
        .push(icons::icon(
            if selected {
                Icon::StarFilled
            } else {
                Icon::Star
            },
            16.0,
            if selected {
                Tone::Fixed(theme::STAR)
            } else {
                Tone::Muted
            },
        ));
    if selected {
        content = content.push(text("Favoris").size(13).font(MEDIUM));
    }
    let tab = button(content)
        .padding(if selected { [6, 9] } else { [6, 7] })
        .on_press(Message::StarredSelected)
        .style(theme::chip(selected));
    if selected {
        tab.into()
    } else {
        tip(tab, "Favoris", tooltip::Position::Bottom)
    }
}

/// The tabs of the inbox, each with its unread lines.
fn tabs(app: &App) -> Element<'_, Message> {
    let mut row = Row::new().spacing(5).align_y(Alignment::Center);
    for category in Category::ALL {
        let selected = app.tab == category;
        let mut content = Row::new()
            .spacing(6)
            .align_y(Alignment::Center)
            .push(text(tab_name(category)).size(13).font(MEDIUM));
        let unread = app.tab_unread.get(&category).copied().unwrap_or(0);
        if unread > 0 {
            content = content.push(text(unread.to_string()).size(12).style(theme::muted));
        }
        row = row.push(
            button(content)
                .padding([5, 10])
                .on_press(Message::TabSelected(category))
                .style(theme::chip(selected)),
        );
    }
    row.into()
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

/// "Tout sélectionner" (the loaded lines), with the count once some are
/// checked.
fn select_bar(app: &App) -> Element<'_, Message> {
    let label = match app.checked.len() {
        0 => "Tout sélectionner".to_string(),
        count => selection_label(count, app.lines_are_threads()),
    };
    container(
        checkbox(app.all_checked())
            .label(label)
            .on_toggle(Message::SelectAll)
            .size(15)
            .text_size(12)
            .spacing(10),
    )
    .padding([8, 20])
    .into()
}

/// "3 conversations sélectionnées", "1 message sélectionné"…
pub fn selection_label(count: usize, threads: bool) -> String {
    match (count > 1, threads) {
        (true, true) => format!("{count} conversations sélectionnées"),
        (false, true) => format!("{count} conversation sélectionnée"),
        (true, false) => format!("{count} messages sélectionnés"),
        (false, false) => format!("{count} message sélectionné"),
    }
}

fn messages(app: &App) -> Element<'_, Message> {
    if app.items.is_empty() {
        return empty(app);
    }
    let now = Local::now();
    let selected = app.selected_index();
    let mut lines = Column::new().spacing(2).padding([8, 8]);
    for (index, item) in app.items.iter().enumerate() {
        let checked = app.is_checked(item);
        lines = lines.push(message_row(
            app,
            index,
            item,
            selected == Some(index) || checked,
            checked,
            now,
        ));
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
        None if app.sender.is_some() => (
            "Aucun message".to_string(),
            "Aucun message de cet expéditeur n'est enregistré sur cet ordinateur.",
        ),
        None if app.starred => (
            "Aucun favori".to_string(),
            "Ajoutez une étoile à un message (S) pour le retrouver ici.",
        ),
        None if app.is_syncing() => (
            "Synchronisation…".to_string(),
            "Les messages apparaissent dès qu'ils arrivent.",
        ),
        None if app.tabs_shown() => ("Aucun message".to_string(), "Cet onglet est vide."),
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

/// Who wrote the line: the sender of a message, or the people of a
/// conversation, oldest first ("Marie, moi"), by first name when they are
/// several; past three, the first one then the last two.
pub fn sender_line(item: &UnifiedItem) -> String {
    if item.thread_id.is_none() || item.senders.len() <= 1 {
        return item
            .senders
            .first()
            .cloned()
            .unwrap_or_else(|| item.from_display.clone());
    }
    let short: Vec<&str> = item.senders.iter().map(|s| first_name(s)).collect();
    match short.as_slice() {
        [first, .., before_last, last] if short.len() > 3 => {
            format!("{first} … {before_last}, {last}")
        }
        _ => short.join(", "),
    }
}

/// "Marie" for "Marie Durand", "paul" for "paul@ex.fr".
fn first_name(name: &str) -> &str {
    let name = name.trim();
    match name.split_once('@') {
        Some((local, _)) if !name.contains(' ') => local,
        _ => name.split_whitespace().next().unwrap_or(name),
    }
}

fn message_row<'a>(
    app: &'a App,
    index: usize,
    item: &'a UnifiedItem,
    is_selected: bool,
    checked: bool,
    now: DateTime<Local>,
) -> Element<'a, Message> {
    let unread = !item.seen;
    let colors: Vec<_> = item
        .accounts
        .iter()
        .filter_map(|id| find_account(&app.accounts, *id))
        .map(|a| to_color(a.color))
        .collect();

    let sender = text(ellipsize(&sender_line(item), 30))
        .size(14)
        .font(if unread { SEMIBOLD } else { MEDIUM })
        .wrapping(Wrapping::None);
    let mut who = Row::new()
        .spacing(6)
        .align_y(Alignment::Center)
        .push(container(sender).clip(true));
    if item.count > 1 {
        who = who.push(text(item.count.to_string()).size(12).style(theme::muted));
    }
    let mut meta = Row::new().spacing(6).align_y(Alignment::Center);
    if item.flagged {
        // A click on the star takes it off.
        meta = meta.push(tip(
            button(icons::icon(
                Icon::StarFilled,
                13.0,
                Tone::Fixed(theme::STAR),
            ))
            .padding(0)
            .on_press(Message::StarLine(index))
            .style(theme::ghost),
            "Retirer des favoris",
            tooltip::Position::Bottom,
        ));
    }
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
    let first = row![container(who).width(Fill).clip(true), meta]
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
                text(ellipsize(&item.snippet, 42))
                    .size(12)
                    .style(theme::muted)
                    .wrapping(Wrapping::None),
            )
            .clip(true),
        );
    }

    // A checked line shows a tick instead of the avatar.
    let picture = if checked {
        container(icons::icon(
            Icon::Check,
            18.0,
            Tone::Fixed(iced::Color::WHITE),
        ))
        .center(36)
        .style(theme::disc(app.tokens().accent))
        .into()
    } else {
        shared_avatar(&item.from_display, &colors, 36.0)
    };
    let content = row![picture, lines].spacing(12).align_y(Alignment::Start);

    button(content)
        .width(Fill)
        .padding([10, 12])
        .on_press(Message::MessageSelected(index))
        .style(theme::list_row(is_selected))
        .into()
}
