//! The list column of the card: the title of the place shown and its
//! unread count, the tabs of the inbox as pills, account problems, then
//! the lines on three levels (senders, subject, start of the text), or,
//! in "Programmés", the mail waiting to be sent. Under search results,
//! "Chercher aussi sur le serveur". Places, spaces and accounts are in
//! the side bar.

use super::{dot, ellipsize, find_account, format_date, hr, tip, to_color};
use crate::app::{
    AccountMsg, App, LaterMsg, Message, PAGE, SEARCH_ID, ServerSearch, scheduled_status,
    server_search_result, tab_name,
};
use crate::icons::{self, Icon, Tone};
use crate::later;
use crate::theme::{self, BOLD, MEDIUM, SEMIBOLD};
use crate::worker::ErrorKind;
use avero_core::{
    AccountColor, AccountFilter, AuthKind, Category, DraftRecord, FolderRole, Space, UnifiedItem,
};
use chrono::{DateTime, Local};
use iced::widget::text::Wrapping;
use iced::widget::{
    Column, Row, Space as Gap, button, checkbox, column, container, mouse_area, row, scrollable,
    stack, text, text_input, tooltip,
};
use iced::{Alignment, Element, Fill, Padding};

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

/// Width of the list column in this window: 42 % of the card, between
/// 320 and 460 pixels.
pub fn width(app: &App) -> f32 {
    (super::card_width(app) * 0.42).clamp(320.0, 460.0)
}

pub fn view(app: &App) -> Element<'_, Message> {
    let mut content = column![title_row(app), hr()];
    // A folded side bar has no room for the search field: it comes here.
    if super::sidebar::folded(app) {
        content = content
            .push(container(search(app)).padding([10, 14]))
            .push(hr());
    }
    if app.tabs_shown() {
        content = content.push(tabs(app)).push(hr());
    }
    for banner in banners(app) {
        content = content.push(container(banner).padding([10, 14]));
    }
    if !app.checked.is_empty() {
        content = content.push(select_bar(app)).push(hr());
    }
    let content = content.push(messages(app));
    row![
        container(content)
            .width(width(app))
            .height(Fill)
            .style(theme::list_pane),
        super::vr(),
    ]
    .into()
}

/// The title of what the list shows.
fn title(app: &App) -> String {
    match (&app.search_query, &app.sender) {
        (Some(_), _) => "Recherche".to_string(),
        (None, Some(sender)) => format!("Mails de {}", ellipsize(&sender.name, 24)),
        (None, None) if app.snoozed => "Plus tard".to_string(),
        (None, None) if app.scheduled => "Programmés".to_string(),
        (None, None) if app.starred => "Favoris".to_string(),
        (None, None) => folder_name(app.folder).to_string(),
    }
}

/// Unread lines of what is shown: exact for the inbox, the loaded lines
/// elsewhere.
fn unread_shown(app: &App) -> usize {
    if app.in_folder() && app.folder == FolderRole::Inbox {
        match app.scope() {
            AccountFilter::Only(id) => app.unread.get(&id).copied().unwrap_or(0),
            _ => app.space_unread(app.space),
        }
    } else if app.scheduled_shown() {
        0
    } else {
        app.items.iter().filter(|item| !item.seen).count()
    }
}

/// "Réception", "9 non lus", then refresh and "Tout sélectionner".
fn title_row(app: &App) -> Element<'_, Message> {
    let syncing = app.is_syncing();
    let refresh_label = if syncing {
        "Synchronisation en cours…"
    } else {
        "Actualiser"
    };
    let refresh = tip(
        button(icons::icon_rotated(
            Icon::Refresh,
            16.0,
            if syncing { Tone::Accent } else { Tone::Muted },
            if syncing { app.spinner } else { 0.0 },
        ))
        .padding(7)
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
    let subtitle = match (&app.search_query, &app.sender) {
        (Some(query), _) => Some(format!("« {} »", ellipsize(query, 30))),
        (None, Some(sender)) if sender.addr != sender.name => Some(sender.addr.clone()),
        _ => None,
    };
    let mut heading = Column::new().spacing(1).width(Fill).push(
        container(
            text(title(app))
                .size(19)
                .font(BOLD)
                .wrapping(Wrapping::None),
        )
        .clip(true),
    );
    if let Some(subtitle) = subtitle {
        heading = heading.push(
            text(subtitle)
                .size(12)
                .style(theme::muted)
                .wrapping(Wrapping::None),
        );
    }
    line = line.push(heading);
    let unread = unread_shown(app);
    if unread > 0 {
        line = line.push(
            text(if unread > 1 {
                format!("{unread} non lus")
            } else {
                "1 non lu".to_string()
            })
            .size(13)
            .font(SEMIBOLD)
            .style(theme::muted),
        );
    }
    line = line.push(refresh);
    if !app.items.is_empty() && !app.scheduled_shown() {
        line = line.push(tip(
            button(icons::icon(Icon::Check, 16.0, Tone::Muted))
                .padding(7)
                .on_press(Message::SelectAll(true))
                .style(theme::ghost),
            "Tout sélectionner",
            tooltip::Position::Bottom,
        ));
    }
    container(line)
        .height(60)
        .padding(Padding {
            top: 0.0,
            right: 12.0,
            bottom: 0.0,
            left: 20.0,
        })
        .center_y(60)
        .into()
}

/// The search field, in the list when the side bar is folded.
fn search(app: &App) -> Element<'_, Message> {
    let input = text_input("Rechercher (Ctrl+K)", &app.search_text)
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
        .style(theme::input);
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

/// The colour of a space (an index in the accounts' palette).
pub fn space_color(space: &Space) -> iced::Color {
    to_color(AccountColor::palette(space.color))
}

/// The tabs of the inbox as pills: "Important", "Newsletters · 4"…
fn tabs(app: &App) -> Element<'_, Message> {
    let mut row = Row::new().spacing(6).align_y(Alignment::Center);
    for category in Category::ALL {
        let selected = app.tab == category;
        let unread = app.tab_unread.get(&category).copied().unwrap_or(0);
        let label = if unread > 0 {
            format!("{} · {unread}", tab_name(category))
        } else {
            tab_name(category).to_string()
        };
        row = row.push(
            button(
                text(label)
                    .size(13)
                    .font(if selected { BOLD } else { SEMIBOLD })
                    .wrapping(Wrapping::None),
            )
            .height(30)
            .padding([6, 12])
            .on_press(Message::TabSelected(category))
            .style(theme::chip(selected)),
        );
    }
    // A narrow column scrolls the pills sideways rather than wrapping them.
    container(scrollable(row).direction(scrollable::Direction::Horizontal(
        scrollable::Scrollbar::new().width(0).scroller_width(0),
    )))
    .height(44)
    .padding([0, 14])
    .center_y(44)
    .into()
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
            .size(14)
            .text_size(12)
            .spacing(12),
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
    if app.scheduled_shown() {
        return scheduled(app);
    }
    if app.items.is_empty() {
        return empty(app);
    }
    let now = Local::now();
    let selected = app.selected_index();
    let mut lines = Column::new();
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
    if app.search_query.is_some() {
        lines = lines.push(server_search(app));
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

/// Under search results: "Chercher aussi sur le serveur", then how the
/// search on the servers goes.
fn server_search(app: &App) -> Element<'_, Message> {
    let content: Element<'_, Message> = match &app.server_search {
        ServerSearch::Idle => button(
            row![
                icons::icon(Icon::Globe, 15.0, Tone::Accent),
                text("Chercher aussi sur le serveur").size(13).font(MEDIUM),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([8, 14])
        .on_press(Message::SearchServer)
        .style(theme::secondary)
        .into(),
        ServerSearch::Running { .. } => row![
            icons::icon_rotated(Icon::Loader, 15.0, Tone::Accent, app.spinner),
            text("Recherche sur le serveur…")
                .size(13)
                .style(theme::muted),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into(),
        ServerSearch::Done { added, failed, .. } => row![
            icons::icon(
                if *failed > 0 {
                    Icon::Alert
                } else {
                    Icon::Check
                },
                15.0,
                Tone::Muted
            ),
            text(server_search_result(*added, *failed))
                .size(13)
                .style(theme::muted),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into(),
    };
    container(content).padding(12).center_x(Fill).into()
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
        None if app.snoozed => (
            "Rien pour plus tard".to_string(),
            "« Plus tard » (H) range une conversation jusqu'au moment choisi.",
        ),
        None if app.is_syncing() => (
            "Synchronisation…".to_string(),
            "Les messages apparaissent dès qu'ils arrivent.",
        ),
        None if app.tabs_shown() => ("Aucun message".to_string(), "Cet onglet est vide."),
        None => ("Aucun message".to_string(), "Ce dossier est vide."),
    };
    let mut page = column![
        icons::icon(Icon::Inbox, 32.0, Tone::Muted),
        text(title).size(15).font(SEMIBOLD),
        text(detail)
            .size(13)
            .style(theme::muted)
            .align_x(Alignment::Center),
    ]
    .spacing(10)
    .align_x(Alignment::Center)
    .max_width(280);
    if app.search_query.is_some() {
        page = page.push(server_search(app));
    }
    container(page)
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

/// Room on the left of a line: its unread dot, or its checkbox.
const LEAD: f32 = 18.0;

fn message_row<'a>(
    app: &'a App,
    index: usize,
    item: &'a UnifiedItem,
    is_selected: bool,
    checked: bool,
    now: DateTime<Local>,
) -> Element<'a, Message> {
    let unread = !item.seen;
    // Characters that fit on a line of the column (Inter at 14 px).
    let fits = ((width(app) - 2.0 * 20.0 - LEAD) / 7.4).max(12.0) as usize;

    // The checkbox shows under the mouse and while lines are checked.
    let lead: Element<'a, Message> =
        if checked || !app.checked.is_empty() || app.hovered_line == Some(index) {
            checkbox(checked)
                .on_toggle(move |_| Message::LineChecked(index))
                .size(14)
                .into()
        } else if unread {
            dot(app.tokens().unread, 7.0)
        } else {
            Gap::new().width(7).height(7).into()
        };
    let lead = container(lead).width(LEAD).align_y(Alignment::Center);

    let sender = text(ellipsize(&sender_line(item), fits.saturating_sub(14)))
        .size(14)
        .font(if unread { BOLD } else { SEMIBOLD })
        .wrapping(Wrapping::None);
    let mut first = Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(lead)
        .push(container(sender).clip(true));
    if item.count > 1 {
        first = first.push(
            container(text(item.count.to_string()).size(11).font(MEDIUM))
                .padding([0, 7])
                .style(theme::count_pill),
        );
    }
    first = first.push(Gap::new().width(Fill).height(1));
    if item.flagged {
        // A click on the star takes it off.
        first = first.push(tip(
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
        first = first.push(icons::icon(Icon::Paperclip, 13.0, Tone::Muted));
    }
    // With several accounts shown, a small dot says which one got it.
    if app.scope_has_several_accounts() {
        for id in &item.accounts {
            if let Some(account) = find_account(&app.accounts, *id) {
                first = first.push(dot(to_color(account.color), 6.0));
            }
        }
    }
    // In "Plus tard": when it comes back, instead of its date.
    if let Some(until) = item.snoozed_until {
        first = first.push(icons::icon(Icon::AlarmClock, 13.0, Tone::Accent));
        first = first.push(
            text(later::short_local(until, now.timestamp()))
                .size(12)
                .font(MEDIUM)
                .style(theme::accent),
        );
    } else {
        first = first.push(
            text(format_date(item.date, now))
                .size(12)
                .font(if unread { SEMIBOLD } else { theme::INTER })
                .style(theme::muted),
        );
    }

    let indent = Padding::new(0.0).left(LEAD + 8.0);
    let mut lines = Column::new().spacing(3).width(Fill).push(first).push(
        container(
            text(ellipsize(&item.subject, fits))
                .size(14)
                .font(if unread { SEMIBOLD } else { theme::INTER })
                .wrapping(Wrapping::None),
        )
        .padding(indent)
        .clip(true),
    );
    if !item.snippet.is_empty() {
        lines = lines.push(
            container(
                text(ellipsize(&item.snippet, fits))
                    .size(13)
                    .style(theme::muted)
                    .wrapping(Wrapping::None),
            )
            .padding(indent)
            .clip(true),
        );
    }

    let line = button(lines)
        .width(Fill)
        .padding(Padding {
            top: 12.0,
            right: 20.0,
            bottom: 13.0,
            left: 20.0,
        })
        .on_press(Message::MessageSelected(index))
        .style(theme::list_row(is_selected));
    column![
        mouse_area(line)
            .on_enter(Message::LineHovered(index, true))
            .on_exit(Message::LineHovered(index, false)),
        hr(),
    ]
    .into()
}

/// "Programmés": the mail waiting for its time, the next to leave
/// first, then the mail that left late (Avero was closed at its time).
fn scheduled(app: &App) -> Element<'_, Message> {
    if app.outbox.is_empty() {
        return container(
            column![
                icons::icon(Icon::CalendarClock, 32.0, Tone::Muted),
                text("Aucun envoi programmé").size(15).font(SEMIBOLD),
                text("Dans le composeur, « Envoyer plus tard » garde un message ici jusqu'à l'heure choisie.")
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
        .into();
    }
    let now = Local::now().timestamp();
    let mut lines = Column::new().spacing(8).padding([10, 12]);
    for record in &app.outbox {
        lines = lines.push(scheduled_row(app, record, now));
    }
    scrollable(lines)
        .height(Fill)
        .style(theme::scrollbar)
        .into()
}

/// One scheduled mail: to whom, its subject, how it is doing, and what
/// can be done with it.
fn scheduled_row<'a>(app: &'a App, record: &'a DraftRecord, now: i64) -> Element<'a, Message> {
    let sending = app.sending.contains(&record.id);
    let f = &record.fields;
    let to = match f.to.trim() {
        "" => "(sans destinataire)".to_string(),
        to => format!("À : {to}"),
    };
    let subject = match f.subject.trim() {
        "" => "(sans objet)",
        subject => subject,
    };
    let color = find_account(&app.accounts, f.account).map(|a| to_color(a.color));
    let mut head = Row::new().spacing(8).align_y(Alignment::Center);
    if let Some(color) = color {
        head = head.push(dot(color, 8.0));
    }
    head = head.push(
        container(
            text(ellipsize(&to, 40))
                .size(13)
                .font(SEMIBOLD)
                .wrapping(Wrapping::None),
        )
        .width(Fill)
        .clip(true),
    );
    let status = scheduled_status(record, now, sending);
    let (icon, tone, style): (Icon, Tone, fn(&iced::Theme) -> text::Style) =
        if record.error.is_some() {
            (Icon::Alert, Tone::Danger, theme::danger_text)
        } else if record.sent_at.is_some() {
            (Icon::Alert, Tone::Accent, theme::accent)
        } else {
            (Icon::CalendarClock, Tone::Muted, theme::muted)
        };
    let status = row![
        icons::icon(icon, 14.0, tone),
        text(status).size(12).style(style),
    ]
    .spacing(6)
    .align_y(Alignment::Start);
    let action = |label: &'a str, message: Option<Message>| {
        button(text(label).size(12).font(MEDIUM))
            .padding([5, 10])
            .on_press_maybe(message)
            .style(theme::secondary)
    };
    let idle = !sending;
    let actions = if record.sent_at.is_some() {
        row![action(
            "Masquer",
            Some(Message::Later(LaterMsg::Dismiss(record.id)))
        )]
    } else {
        row![
            action(
                "Modifier",
                idle.then_some(Message::Later(LaterMsg::Edit(record.id)))
            ),
            action(
                "Envoyer maintenant",
                idle.then_some(Message::Later(LaterMsg::SendNow(record.id)))
            ),
            action(
                "Annuler l'envoi",
                idle.then_some(Message::Later(LaterMsg::Cancel(record.id)))
            ),
        ]
    }
    .spacing(6);
    container(
        column![
            head,
            text(ellipsize(subject, 44))
                .size(13)
                .wrapping(Wrapping::None),
            status,
            actions,
        ]
        .spacing(6),
    )
    .width(Fill)
    .padding([10, 12])
    .style(theme::panel)
    .into()
}
